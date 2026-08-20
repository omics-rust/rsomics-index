use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::mem;
use std::num::NonZero;
use std::path::Path;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender};
use libdeflater::Decompressor;
use memchr::memchr;
use noodles_bgzf::VirtualPosition;
use rsomics_common::{Context, Result, RsomicsError};

use noodles::csi::binning_index::index::reference_sequence::bin::Chunk;

use crate::bgzip::frame::{EOF_BLOCK, decode_frame, read_frame};

#[derive(Clone)]
struct Block {
    compressed: u64,
    next: u64,
    data: Arc<[u8]>,
    eof: bool,
}

pub(super) struct BlockReader {
    file: File,
    inline: Option<Decompressor>,
    pool: Option<DecoderPool>,
    cache: BlockCache,
    next_job: u64,
    ready: HashMap<u64, io::Result<Arc<[u8]>>>,
    workers: usize,
    #[cfg(test)]
    stats: ReaderStats,
}

#[cfg(test)]
#[derive(Default)]
struct ReaderStats {
    decoded_blocks: u64,
    cache_hits: u64,
    peak_pending: usize,
}

impl BlockReader {
    pub fn open(path: &Path, workers: NonZero<usize>, cache_bytes: usize) -> Result<Self> {
        let mut file =
            File::open(path).rs_with_context(|| format!("opening BGZF data {}", path.display()))?;
        validate_tail(&mut file)?;
        let (inline, pool) = if workers.get() == 1 {
            (Some(Decompressor::new()), None)
        } else {
            (None, Some(DecoderPool::new(workers)?))
        };
        Ok(Self {
            file,
            inline,
            pool,
            cache: BlockCache::new(cache_bytes),
            next_job: 0,
            ready: HashMap::new(),
            workers: workers.get(),
            #[cfg(test)]
            stats: ReaderStats::default(),
        })
    }

    pub fn read_chunks<F>(&mut self, chunks: &[Chunk], mut visit: F) -> Result<()>
    where
        F: FnMut(VirtualPosition, &[u8]) -> Result<()>,
    {
        for chunk in chunks {
            if chunk.end() < chunk.start() {
                return Err(invalid("index chunk end is before its start"));
            }
            let mut line = Vec::new();
            let mut line_offset = None;
            let start = chunk.start();
            let end = chunk.end();
            self.visit_range(start.compressed(), end, |block| {
                let slice_start = if block.compressed == start.compressed() {
                    usize::from(start.uncompressed())
                } else {
                    0
                };
                let slice_end = if block.compressed == end.compressed() {
                    usize::from(end.uncompressed())
                } else {
                    block.data.len()
                };
                if slice_start > slice_end || slice_end > block.data.len() {
                    return Err(invalid("index chunk has an invalid uncompressed offset"));
                }
                feed_lines(
                    block,
                    slice_start,
                    slice_end,
                    &mut line,
                    &mut line_offset,
                    &mut visit,
                )
            })?;
            if !line.is_empty() {
                let offset = line_offset.ok_or_else(|| invalid("record offset is missing"))?;
                visit(offset, &line)?;
            }
        }
        Ok(())
    }

    pub fn read_all<F>(&mut self, mut visit: F) -> Result<()>
    where
        F: FnMut(VirtualPosition, &[u8]) -> Result<()>,
    {
        let mut line = Vec::new();
        let mut line_offset = None;
        self.visit_all(|block| {
            feed_lines(
                block,
                0,
                block.data.len(),
                &mut line,
                &mut line_offset,
                &mut visit,
            )
        })?;
        if !line.is_empty() {
            visit(
                line_offset.ok_or_else(|| invalid("record offset is missing"))?,
                &line,
            )?;
        }
        Ok(())
    }

    fn visit_range<F>(&mut self, start: u64, end: VirtualPosition, visit: F) -> Result<()>
    where
        F: FnMut(&Block) -> Result<()>,
    {
        if start > end.compressed() {
            return Err(invalid("index chunk compressed offsets are inverted"));
        }
        self.visit_blocks(start, Some(end), visit)
    }

    fn visit_all<F>(&mut self, visit: F) -> Result<()>
    where
        F: FnMut(&Block) -> Result<()>,
    {
        self.visit_blocks(0, None, visit)
    }

    fn visit_blocks<F>(
        &mut self,
        start: u64,
        end: Option<VirtualPosition>,
        mut visit: F,
    ) -> Result<()>
    where
        F: FnMut(&Block) -> Result<()>,
    {
        let mut pending = VecDeque::new();
        let mut next = start;
        let mut scheduling_done = false;
        let mut saw_eof = false;

        loop {
            while pending.len() < self.workers && !scheduling_done {
                if end.is_some_and(|end| !needs_block(next, end)) {
                    scheduling_done = true;
                    break;
                }
                let (scheduled, block_next, eof) = self.schedule(next)?;
                if let Some(end) = end
                    && block_next > end.compressed()
                    && next != end.compressed()
                {
                    return Err(invalid("index chunk end is not at a BGZF block boundary"));
                }
                next = block_next;
                pending.push_back(scheduled);
                #[cfg(test)]
                {
                    self.stats.peak_pending = self.stats.peak_pending.max(pending.len());
                }
                if eof {
                    scheduling_done = true;
                }
            }

            let Some(scheduled) = pending.pop_front() else {
                break;
            };
            let block = self.resolve(scheduled)?;
            if block.eof {
                saw_eof = true;
                if let Some(end) = end
                    && needs_block(block.compressed, end)
                {
                    return Err(invalid("index chunk reaches the BGZF EOF block"));
                }
            } else {
                visit(&block)?;
            }
        }

        match end {
            Some(end) if needs_block(next, end) => {
                Err(invalid("index chunk extends beyond the BGZF stream"))
            }
            Some(_) => Ok(()),
            None if !saw_eof => Err(RsomicsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "BGZF data is missing its complete EOF marker",
            ))),
            None => {
                let length = self.file.metadata()?.len();
                if next == length {
                    Ok(())
                } else {
                    Err(invalid("bytes found after the BGZF EOF marker"))
                }
            }
        }
    }

    fn schedule(&mut self, compressed: u64) -> Result<(Scheduled, u64, bool)> {
        if let Some(block) = self.cache.get(compressed) {
            #[cfg(test)]
            {
                self.stats.cache_hits += 1;
            }
            let next = block.next;
            let eof = block.eof;
            return Ok((Scheduled::Ready(block), next, eof));
        }
        self.file.seek(SeekFrom::Start(compressed))?;
        let frame = read_frame(&mut self.file)
            .rs_with_context(|| format!("reading BGZF block at compressed offset {compressed}"))?;
        let frame_len =
            u64::try_from(frame.len()).map_err(|_| invalid("BGZF frame length exceeds u64"))?;
        let next = compressed
            .checked_add(frame_len)
            .ok_or_else(|| invalid("BGZF compressed offset overflows u64"))?;
        let eof = frame == EOF_BLOCK;
        if let Some(decompressor) = &mut self.inline {
            let data = decode_frame(decompressor, &frame).rs_with_context(|| {
                format!("decoding BGZF block at compressed offset {compressed}")
            })?;
            let block = Block {
                compressed,
                next,
                data: Arc::from(data),
                eof,
            };
            self.cache.insert(block.clone());
            #[cfg(test)]
            {
                self.stats.decoded_blocks += 1;
            }
            return Ok((Scheduled::Ready(block), next, eof));
        }
        let id = self.next_job;
        self.next_job = self
            .next_job
            .checked_add(1)
            .ok_or_else(|| invalid("BGZF decode job count overflows u64"))?;
        self.pool
            .as_ref()
            .ok_or_else(|| invalid("BGZF decoder is missing"))?
            .send(DecodeJob { id, frame })?;
        #[cfg(test)]
        {
            self.stats.decoded_blocks += 1;
        }
        Ok((
            Scheduled::Job {
                id,
                compressed,
                next,
                eof,
            },
            next,
            eof,
        ))
    }

    fn resolve(&mut self, scheduled: Scheduled) -> Result<Block> {
        match scheduled {
            Scheduled::Ready(block) => Ok(block),
            Scheduled::Job {
                id,
                compressed,
                next,
                eof,
            } => {
                let data = loop {
                    if let Some(result) = self.ready.remove(&id) {
                        break result.rs_with_context(|| {
                            format!("decoding BGZF block at compressed offset {compressed}")
                        })?;
                    }
                    let result = self
                        .pool
                        .as_ref()
                        .ok_or_else(|| invalid("BGZF decoder is missing"))?
                        .receive()?;
                    if result.id == id {
                        break result.data.rs_with_context(|| {
                            format!("decoding BGZF block at compressed offset {compressed}")
                        })?;
                    }
                    self.ready.insert(result.id, result.data);
                };
                let block = Block {
                    compressed,
                    next,
                    data,
                    eof,
                };
                self.cache.insert(block.clone());
                Ok(block)
            }
        }
    }
}

enum Scheduled {
    Ready(Block),
    Job {
        id: u64,
        compressed: u64,
        next: u64,
        eof: bool,
    },
}

struct BlockCache {
    capacity: usize,
    used: usize,
    values: HashMap<u64, Block>,
    order: VecDeque<u64>,
}

impl BlockCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            used: 0,
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, compressed: u64) -> Option<Block> {
        let block = self.values.get(&compressed)?.clone();
        self.order.retain(|key| *key != compressed);
        self.order.push_back(compressed);
        Some(block)
    }

    fn insert(&mut self, block: Block) {
        let weight = block_weight(&block);
        if self.capacity == 0 || weight > self.capacity {
            return;
        }
        if let Some(previous) = self.values.remove(&block.compressed) {
            self.used -= block_weight(&previous);
            self.order.retain(|key| *key != block.compressed);
        }
        while self
            .used
            .checked_add(weight)
            .is_none_or(|total| total > self.capacity)
        {
            let Some(key) = self.order.pop_front() else {
                break;
            };
            if let Some(previous) = self.values.remove(&key) {
                self.used -= block_weight(&previous);
            }
        }
        self.used = self
            .used
            .checked_add(weight)
            .expect("cache eviction makes the inserted weight fit");
        self.order.push_back(block.compressed);
        self.values.insert(block.compressed, block);
    }
}

fn block_weight(block: &Block) -> usize {
    block.data.len().saturating_add(mem::size_of::<Block>())
}

struct DecodeJob {
    id: u64,
    frame: Vec<u8>,
}

struct DecodeResult {
    id: u64,
    data: io::Result<Arc<[u8]>>,
}

struct DecoderPool {
    jobs: Option<Sender<DecodeJob>>,
    results: Receiver<DecodeResult>,
    handles: Vec<JoinHandle<()>>,
}

impl DecoderPool {
    fn new(workers: NonZero<usize>) -> Result<Self> {
        let (job_tx, job_rx) = crossbeam_channel::bounded(workers.get());
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        let mut handles = Vec::with_capacity(workers.get());
        for index in 0..workers.get() {
            let jobs = job_rx.clone();
            let results = result_tx.clone();
            match thread::Builder::new()
                .name(format!("rsomics-index-decode-{index}"))
                .spawn(move || decode_worker(jobs, results))
            {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    drop(job_tx);
                    for handle in handles {
                        let _ = handle.join();
                    }
                    return Err(RsomicsError::Io(error));
                }
            }
        }
        drop(result_tx);
        Ok(Self {
            jobs: Some(job_tx),
            results: result_rx,
            handles,
        })
    }

    fn send(&self, job: DecodeJob) -> Result<()> {
        self.jobs
            .as_ref()
            .ok_or_else(|| invalid("BGZF decoder is closed"))?
            .send(job)
            .map_err(|_| invalid("BGZF decoder stopped"))
    }

    fn receive(&self) -> Result<DecodeResult> {
        self.results
            .recv()
            .map_err(|_| invalid("BGZF decoder stopped before returning a block"))
    }
}

impl Drop for DecoderPool {
    fn drop(&mut self) {
        self.jobs.take();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn decode_worker(jobs: Receiver<DecodeJob>, results: Sender<DecodeResult>) {
    let mut decompressor = Decompressor::new();
    while let Ok(job) = jobs.recv() {
        let data = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_frame(&mut decompressor, &job.frame).map(Arc::from)
        }))
        .unwrap_or_else(|_| Err(io::Error::other("BGZF decoder worker panicked")));
        if results.send(DecodeResult { id: job.id, data }).is_err() {
            break;
        }
    }
}

fn feed_lines<F>(
    block: &Block,
    start: usize,
    end: usize,
    line: &mut Vec<u8>,
    line_offset: &mut Option<VirtualPosition>,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(VirtualPosition, &[u8]) -> Result<()>,
{
    let mut position = start;
    while position < end {
        let remaining = &block.data[position..end];
        let newline = memchr(b'\n', remaining);
        if line.is_empty() {
            let uncompressed =
                u16::try_from(position).map_err(|_| invalid("BGZF line offset exceeds u16"))?;
            let offset = VirtualPosition::new(block.compressed, uncompressed)
                .ok_or_else(|| invalid("BGZF line offset is invalid"))?;
            match newline {
                Some(relative_end) => {
                    let next = position + relative_end + 1;
                    visit(offset, &block.data[position..next])?;
                    position = next;
                }
                None => {
                    *line_offset = Some(offset);
                    line.extend_from_slice(remaining);
                    position = end;
                }
            }
        } else {
            match newline {
                Some(relative_end) => {
                    let next = position + relative_end + 1;
                    line.extend_from_slice(&block.data[position..next]);
                    visit(
                        line_offset
                            .take()
                            .ok_or_else(|| invalid("record offset is missing"))?,
                        line,
                    )?;
                    line.clear();
                    position = next;
                }
                None => {
                    line.extend_from_slice(remaining);
                    position = end;
                }
            }
        }
    }
    Ok(())
}

fn needs_block(compressed: u64, end: VirtualPosition) -> bool {
    compressed < end.compressed() || (compressed == end.compressed() && end.uncompressed() != 0)
}

fn validate_tail(file: &mut File) -> Result<()> {
    let length = file.metadata()?.len();
    let eof_size = u64::try_from(EOF_BLOCK.len()).map_err(|_| invalid("EOF size exceeds u64"))?;
    if length < eof_size {
        return Err(RsomicsError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "BGZF data is missing its complete EOF marker",
        )));
    }
    file.seek(SeekFrom::End(
        -i64::try_from(eof_size).map_err(|_| invalid("EOF size exceeds i64"))?,
    ))?;
    let mut tail = [0; EOF_BLOCK.len()];
    file.read_exact(&mut tail)?;
    if tail != EOF_BLOCK {
        return Err(RsomicsError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "BGZF data is missing its complete EOF marker",
        )));
    }
    if length >= eof_size * 2 {
        file.seek(SeekFrom::End(
            -i64::try_from(eof_size * 2).map_err(|_| invalid("EOF size exceeds i64"))?,
        ))?;
        let mut previous = [0; EOF_BLOCK.len()];
        file.read_exact(&mut previous)?;
        if previous == EOF_BLOCK {
            return Err(invalid("BGZF data has duplicate EOF markers"));
        }
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use crate::bgzip::{CompressOptions, compress};

    #[test]
    fn worker_pipeline_and_cache_are_bounded_and_reused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("records.bed.gz");
        let mut data = Vec::new();
        for position in 0..30_000 {
            writeln!(data, "chr1\t{position}\t{}", position + 1).unwrap();
        }
        let output = File::create(&path).unwrap();
        let (output, _) = compress(data.as_slice(), output, &CompressOptions::default()).unwrap();
        drop(output);
        let eof = std::fs::metadata(&path).unwrap().len() - EOF_BLOCK.len() as u64;
        let chunk = Chunk::new(VirtualPosition::MIN, VirtualPosition::new(eof, 0).unwrap());
        let mut reader =
            BlockReader::open(&path, NonZero::new(4).unwrap(), 2 * 1024 * 1024).unwrap();
        let mut records = 0u64;

        reader
            .read_chunks(&[chunk], |_, _| {
                records += 1;
                Ok(())
            })
            .unwrap();
        let decoded = reader.stats.decoded_blocks;
        reader.read_chunks(&[chunk], |_, _| Ok(())).unwrap();

        assert_eq!(records, 30_000);
        assert!(decoded > 1);
        assert!(reader.stats.peak_pending > 1);
        assert_eq!(reader.stats.decoded_blocks, decoded);
        assert!(reader.stats.cache_hits >= decoded);
        assert!(reader.cache.used <= reader.cache.capacity);

        let mut uncached = BlockReader::open(&path, NonZero::new(2).unwrap(), 0).unwrap();
        uncached.read_chunks(&[chunk], |_, _| Ok(())).unwrap();
        let first_pass = uncached.stats.decoded_blocks;
        uncached.read_chunks(&[chunk], |_, _| Ok(())).unwrap();
        assert_eq!(uncached.stats.decoded_blocks, first_pass * 2);
        assert_eq!(uncached.stats.cache_hits, 0);
        assert_eq!(uncached.cache.used, 0);
    }

    #[test]
    fn one_worker_decodes_without_a_thread_pool() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("records.bed.gz");
        let output = File::create(&path).unwrap();
        let (output, _) = compress(
            b"chr1\t0\t1\n".as_slice(),
            output,
            &CompressOptions::default(),
        )
        .unwrap();
        drop(output);

        let mut reader = BlockReader::open(&path, NonZero::<usize>::MIN, 0).unwrap();
        let mut records = 0;
        reader
            .read_all(|_, _| {
                records += 1;
                Ok(())
            })
            .unwrap();

        assert_eq!(records, 1);
        assert!(reader.inline.is_some());
        assert!(reader.pool.is_none());
    }
}
