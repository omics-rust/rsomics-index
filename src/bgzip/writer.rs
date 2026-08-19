use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded};
use libdeflater::{CompressionLvl, Compressor};

use super::{CompressOptions, StreamStats};

const MAX_DATA_SIZE: usize = 65_239;
const HEADER_SIZE: usize = 18;
const TRAILER_SIZE: usize = 8;
const EOF_BLOCK: [u8; 28] = [
    0x1f, 0x8b, 0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x06, 0x00, b'B', b'C', 0x02, 0x00,
    0x1b, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

struct Work {
    sequence: u64,
    data: Vec<u8>,
}

struct Done {
    sequence: u64,
    frame: io::Result<Vec<u8>>,
}

pub(super) fn compress<R, W>(
    mut source: R,
    sink: W,
    options: &CompressOptions,
) -> io::Result<(W, StreamStats)>
where
    R: Read,
    W: Write + Send + 'static,
{
    if options.level > 9 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "BGZF compression level must be between 0 and 9, got {}",
                options.level
            ),
        ));
    }

    let queue_capacity = options.workers.get().saturating_mul(2).max(1);
    let (work_tx, work_rx) = bounded::<Work>(queue_capacity);
    let (done_tx, done_rx) = bounded::<Done>(queue_capacity);
    let (sink_tx, sink_rx) = mpsc::sync_channel(1);

    let writer = thread::spawn(move || write_frames(sink, done_rx, sink_tx));
    let mut workers = Vec::with_capacity(options.workers.get());
    for _ in 0..options.workers.get() {
        let work_rx = work_rx.clone();
        let done_tx = done_tx.clone();
        let level = options.level;
        workers.push(thread::spawn(move || {
            compress_blocks(level, work_rx, done_tx)
        }));
    }
    drop(work_rx);
    drop(done_tx);

    let mut bytes_in = 0u64;
    let mut blocks = 0u64;
    let mut buffer = vec![0u8; MAX_DATA_SIZE];
    let mut pending = Vec::with_capacity(MAX_DATA_SIZE * 2);
    while let Some(data) = next_block(&mut source, &mut buffer, &mut pending, options.text)? {
        let size = data.len();
        bytes_in = bytes_in
            .checked_add(size as u64)
            .ok_or_else(|| io::Error::other("input byte count overflow"))?;
        work_tx
            .send(Work {
                sequence: blocks,
                data,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "BGZF worker stopped"))?;
        blocks += 1;
    }
    drop(work_tx);

    let mut worker_panicked = false;
    for handle in workers {
        worker_panicked |= handle.join().is_err();
    }
    let writer_result = writer
        .join()
        .map_err(|_| io::Error::other("BGZF writer thread panicked"))?;
    writer_result?;
    if worker_panicked {
        return Err(io::Error::other("BGZF compression worker panicked"));
    }

    let (sink, bytes_out, written_blocks) = sink_rx
        .recv()
        .map_err(|_| io::Error::other("BGZF writer did not return its sink"))??;
    if written_blocks != blocks {
        return Err(io::Error::other(
            "BGZF output is missing a compressed block",
        ));
    }

    Ok((
        sink,
        StreamStats {
            bytes_in,
            bytes_out,
            blocks,
        },
    ))
}

fn next_block<R: Read>(
    source: &mut R,
    buffer: &mut [u8],
    pending: &mut Vec<u8>,
    text: bool,
) -> io::Result<Option<Vec<u8>>> {
    if !text {
        let size = read_full(source, buffer)?;
        return Ok((size > 0).then(|| buffer[..size].to_vec()));
    }

    loop {
        if pending.len() >= MAX_DATA_SIZE {
            let split = pending[..MAX_DATA_SIZE]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(MAX_DATA_SIZE, |position| position + 1);
            let remainder = pending.split_off(split);
            return Ok(Some(std::mem::replace(pending, remainder)));
        }

        let size = source.read(buffer)?;
        if size == 0 {
            return Ok((!pending.is_empty()).then(|| std::mem::take(pending)));
        }
        pending.extend_from_slice(&buffer[..size]);
    }
}

fn read_full<R: Read>(source: &mut R, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..])? {
            0 => break,
            size => filled += size,
        }
    }
    Ok(filled)
}

fn compress_blocks(level: u8, work_rx: Receiver<Work>, done_tx: Sender<Done>) {
    let mut compressor = if level == 0 {
        None
    } else {
        let Ok(compression_level) = CompressionLvl::new(i32::from(level)) else {
            return;
        };
        Some(Compressor::new(compression_level))
    };

    while let Ok(work) = work_rx.recv() {
        let frame = encode_frame(&work.data, compressor.as_mut());
        if done_tx
            .send(Done {
                sequence: work.sequence,
                frame,
            })
            .is_err()
        {
            return;
        }
    }
}

fn encode_frame(data: &[u8], compressor: Option<&mut Compressor>) -> io::Result<Vec<u8>> {
    let compressed = match compressor {
        Some(compressor) => {
            let mut output = vec![0; compressor.deflate_compress_bound(data.len())];
            let size = compressor
                .deflate_compress(data, &mut output)
                .map_err(|error| io::Error::other(format!("deflate failed: {error:?}")))?;
            output.truncate(size);
            output
        }
        None => encode_stored(data)?,
    };

    let block_size = HEADER_SIZE
        .checked_add(compressed.len())
        .and_then(|size| size.checked_add(TRAILER_SIZE))
        .ok_or_else(|| io::Error::other("BGZF block size overflow"))?;
    let bsize = u16::try_from(block_size - 1)
        .map_err(|_| io::Error::other("compressed BGZF block exceeds 64 KiB"))?;

    let mut frame = Vec::with_capacity(block_size);
    frame.extend_from_slice(&[
        0x1f, 0x8b, 0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x06, 0x00, b'B', b'C', 0x02,
        0x00,
    ]);
    frame.extend_from_slice(&bsize.to_le_bytes());
    frame.extend_from_slice(&compressed);
    frame.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
    Ok(frame)
}

fn encode_stored(data: &[u8]) -> io::Result<Vec<u8>> {
    let length = u16::try_from(data.len())
        .map_err(|_| io::Error::other("stored BGZF block exceeds 64 KiB"))?;
    let mut output = Vec::with_capacity(data.len() + 5);
    output.push(0x01);
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(&(!length).to_le_bytes());
    output.extend_from_slice(data);
    Ok(output)
}

fn write_frames<W: Write>(
    mut sink: W,
    done_rx: Receiver<Done>,
    sink_tx: mpsc::SyncSender<io::Result<(W, u64, u64)>>,
) -> io::Result<()> {
    let mut pending = BTreeMap::new();
    let mut next = 0u64;
    let mut bytes_out = 0u64;

    while let Ok(done) = done_rx.recv() {
        pending.insert(done.sequence, done.frame);
        while let Some(frame) = pending.remove(&next) {
            let frame = frame?;
            sink.write_all(&frame)?;
            bytes_out += frame.len() as u64;
            next += 1;
        }
    }

    if !pending.is_empty() {
        return Err(io::Error::other(
            "BGZF compression result sequence has a gap",
        ));
    }
    sink.write_all(&EOF_BLOCK)?;
    sink.flush()?;
    bytes_out += EOF_BLOCK.len() as u64;
    sink_tx
        .send(Ok((sink, bytes_out, next)))
        .map_err(|_| io::Error::other("BGZF caller stopped before receiving its sink"))
}
