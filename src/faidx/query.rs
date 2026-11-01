use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::bounded;
use rsomics_common::Result;

use crate::bgzip::GziIndex;
use crate::reference::invalid;

use super::index::{Index, Record};
use super::reader::Source;
use super::region::Region;

const CHUNK_BASES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
struct Task {
    order: usize,
    region: usize,
    start: u64,
    bases: usize,
}

pub(super) fn run(
    reference: &Path,
    index: Arc<Index>,
    gzi: Option<Arc<GziIndex>>,
    regions: &[Region],
    workers: usize,
    width: Option<u64>,
    output: &mut dyn Write,
) -> Result<u64> {
    let mut tasks = plan(regions);
    let mut bases = 0u64;
    let mut column = 0u64;
    let mut emit = |task: Task, data: Result<Vec<u8>>| -> Result<()> {
        let region = &regions[task.region];
        let wrap = width.unwrap_or(index.records[region.record].line_bases);
        if task.start == region.start {
            writeln!(output, ">{}", region.header)?;
            column = 0;
        }
        wrapped(output, &data?, wrap, &mut column)?;
        if task.start + task.bases as u64 == region.end {
            if column > 0 {
                output.write_all(b"\n")?;
            }
            bases = bases
                .checked_add(region.end - region.start)
                .ok_or_else(|| invalid("query base count overflow"))?;
        }
        Ok(())
    };
    if workers == 1 {
        let mut reader = BufReader::with_capacity(256 * 1024, Source::open(reference, gzi, false)?);
        for task in tasks {
            let data = chunk(
                &mut reader,
                &index.records[regions[task.region].record],
                task.start,
                task.bases,
            );
            emit(task, data)?;
        }
        return Ok(bases);
    }
    let mut readers = Vec::new();
    for _ in 0..workers {
        readers.push(BufReader::with_capacity(
            256 * 1024,
            Source::open(reference, gzi.clone(), false)?,
        ));
    }
    thread::scope(|scope| {
        let capacity = workers
            .checked_mul(2)
            .ok_or_else(|| invalid("query worker count overflow"))?;
        let (send, pending) = bounded::<Task>(capacity);
        let (results, receive) = bounded::<(usize, Result<Vec<u8>>)>(capacity);
        for mut reader in readers {
            let tasks = pending.clone();
            let results = results.clone();
            let index = Arc::clone(&index);
            thread::Builder::new()
                .name("reference-query".to_owned())
                .spawn_scoped(scope, move || {
                    while let Ok(task) = tasks.recv() {
                        let data = chunk(
                            &mut reader,
                            &index.records[regions[task.region].record],
                            task.start,
                            task.bases,
                        );
                        if results.send((task.order, data)).is_err() {
                            break;
                        }
                    }
                })?;
        }
        drop(pending);
        drop(results);
        let outcome = (|| -> Result<()> {
            loop {
                let mut batch: Vec<_> = tasks.by_ref().take(capacity).collect();
                if batch.is_empty() {
                    break;
                }
                for (order, task) in batch.iter_mut().enumerate() {
                    task.order = order;
                    send.send(*task)
                        .map_err(|_| invalid("reference query workers stopped"))?;
                }
                let mut ready = Vec::with_capacity(batch.len());
                for _ in 0..batch.len() {
                    ready.push(
                        receive
                            .recv()
                            .map_err(|_| invalid("reference query worker produced no result"))?,
                    );
                }
                ready.sort_unstable_by_key(|(order, _)| *order);
                for ((_, data), task) in ready.into_iter().zip(batch) {
                    emit(task, data)?;
                }
            }
            Ok(())
        })();
        drop(send);
        outcome
    })?;
    Ok(bases)
}

fn plan(regions: &[Region]) -> impl Iterator<Item = Task> + '_ {
    let mut next_region = 0;
    let mut position = regions.first().map_or(0, |region| region.start);
    std::iter::from_fn(move || {
        let region = regions.get(next_region)?;
        let bases = (region.end - position).min(CHUNK_BASES as u64) as usize;
        let task = Task {
            order: 0,
            region: next_region,
            start: position,
            bases,
        };
        position += bases as u64;
        if position == region.end {
            next_region += 1;
            position = regions.get(next_region).map_or(0, |region| region.start);
        }
        Some(task)
    })
}

fn chunk(
    reader: &mut BufReader<Source>,
    record: &Record,
    start: u64,
    bases: usize,
) -> Result<Vec<u8>> {
    if bases == 0 {
        return Ok(Vec::new());
    }
    let offset = record.byte_offset(start)?;
    reader.seek(SeekFrom::Start(offset))?;
    if !reader.get_ref().compressed() {
        let span = record.byte_offset(start + bases as u64 - 1)? - offset + 1;
        if span < reader.capacity() as u64 {
            let mut bounded = BufReader::with_capacity(span as usize, reader.get_mut().take(span));
            return read_bases(&mut bounded, record, start, bases);
        }
    }
    read_bases(reader, record, start, bases)
}

fn read_bases(
    reader: &mut impl Read,
    record: &Record,
    start: u64,
    bases: usize,
) -> Result<Vec<u8>> {
    let mut data = vec![0; bases];
    let mut written = 0;
    let mut position = start;
    while written < bases {
        let count = (record.line_bases - position % record.line_bases).min((bases - written) as u64)
            as usize;
        reader.read_exact(&mut data[written..written + count])?;
        if data[written..written + count]
            .iter()
            .any(|byte| !byte.is_ascii_graphic())
        {
            return Err(invalid("FAI does not match the reference sequence layout"));
        }
        written += count;
        position += count as u64;
        if written < bases {
            let extra = (record.line_width - record.line_bases) as usize;
            let mut ending = [0; 2];
            reader.read_exact(&mut ending[..extra])?;
            if !matches!(&ending[..extra], b"" | b"\n" | b"\r\n") {
                return Err(invalid(
                    "FAI line width does not match the reference terminator",
                ));
            }
        }
    }
    Ok(data)
}

fn wrapped(output: &mut dyn Write, mut data: &[u8], width: u64, column: &mut u64) -> Result<()> {
    if width == 0 {
        output.write_all(data)?;
        *column = u64::from(!data.is_empty());
        return Ok(());
    }
    while !data.is_empty() {
        let count = (width - *column).min(data.len() as u64) as usize;
        output.write_all(&data[..count])?;
        *column += count as u64;
        data = &data[count..];
        if *column == width {
            output.write_all(b"\n")?;
            *column = 0;
        }
    }
    Ok(())
}
