use std::collections::BTreeMap;
use std::io::{self, BufWriter, Read, Write};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded};
use libdeflater::{CompressionLvl, Compressor};

use super::{CompressOptions, StreamStats, frame::EOF_BLOCK};

const MAX_DATA_SIZE: usize = 65_239;
const HEADER_SIZE: usize = 18;
const TRAILER_SIZE: usize = 8;
const FRAME_SIZE: usize = 65_536;
const OUTPUT_BUFFER_SIZE: usize = 256 * 1024;

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
    if options.workers.get() == 1 {
        return compress_inline(source, sink, options);
    }

    let queue_capacity = options.workers.get().saturating_mul(2).max(1);
    let (work_tx, work_rx) = bounded::<Work>(queue_capacity);
    let (done_tx, done_rx) = bounded::<Done>(queue_capacity);
    let (credit_tx, credit_rx) = bounded(queue_capacity);
    for _ in 0..queue_capacity {
        credit_tx.send(()).expect("new credit queue has capacity");
    }

    let writer = thread::spawn(move || write_frames(sink, done_rx, credit_tx));
    let mut workers = Vec::with_capacity(options.workers.get());
    for _ in 0..options.workers.get() {
        let work_rx = work_rx.clone();
        let done_tx = done_tx.clone();
        let level = options.level;
        workers.push(thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compress_blocks(level, work_rx, &done_tx)
            }));
            if result.is_err() {
                let _ = done_tx.send(Done {
                    sequence: 0,
                    frame: Err(io::Error::other("BGZF compression worker panicked")),
                });
            }
        }));
    }
    drop(work_rx);
    drop(done_tx);

    let producer_result = (|| {
        let mut bytes_in = 0u64;
        let mut blocks = 0u64;
        let mut input = InputBlocks::new();
        while let Some(data) = input.next(&mut source, options.text)? {
            let size = data.len();
            bytes_in = bytes_in
                .checked_add(size as u64)
                .ok_or_else(|| io::Error::other("input byte count overflow"))?;
            credit_rx
                .recv()
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "BGZF writer stopped"))?;
            work_tx
                .send(Work {
                    sequence: blocks,
                    data: data.to_vec(),
                })
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "BGZF worker stopped"))?;
            blocks += 1;
        }
        Ok::<_, io::Error>((bytes_in, blocks))
    })();
    drop(work_tx);

    let mut worker_panicked = false;
    for handle in workers {
        worker_panicked |= handle.join().is_err();
    }
    let writer_result = writer
        .join()
        .map_err(|_| io::Error::other("BGZF writer thread panicked"))?;
    let (sink, bytes_out, written_blocks) = writer_result?;
    if worker_panicked {
        return Err(io::Error::other("BGZF compression worker panicked"));
    }
    let (bytes_in, blocks) = producer_result?;
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

fn compress_inline<R: Read, W: Write>(
    mut source: R,
    sink: W,
    options: &CompressOptions,
) -> io::Result<(W, StreamStats)> {
    let mut sink = BufWriter::with_capacity(OUTPUT_BUFFER_SIZE, sink);
    let mut input = InputBlocks::new();
    let mut frame = vec![0; FRAME_SIZE];
    let mut compressor = compressor(options.level);
    let mut stats = StreamStats {
        bytes_in: 0,
        bytes_out: EOF_BLOCK.len() as u64,
        blocks: 0,
    };
    while let Some(data) = input.next(&mut source, options.text)? {
        let size = encode_frame(data, compressor.as_mut(), &mut frame)?;
        sink.write_all(&frame[..size])?;
        stats.bytes_in = stats
            .bytes_in
            .checked_add(data.len() as u64)
            .ok_or_else(|| io::Error::other("input byte count overflow"))?;
        stats.bytes_out += size as u64;
        stats.blocks += 1;
    }
    sink.write_all(&EOF_BLOCK)?;
    sink.flush()?;
    Ok((
        sink.into_inner().map_err(|error| error.into_error())?,
        stats,
    ))
}

struct InputBlocks {
    data: Vec<u8>,
    start: usize,
    end: usize,
}

impl InputBlocks {
    fn new() -> Self {
        Self {
            data: vec![0; MAX_DATA_SIZE],
            start: 0,
            end: 0,
        }
    }

    fn next<R: Read>(&mut self, source: &mut R, text: bool) -> io::Result<Option<&[u8]>> {
        let remaining = self.end - self.start;
        self.data.copy_within(self.start..self.end, 0);
        self.end = remaining + read_full(source, &mut self.data[remaining..])?;
        if self.end == 0 {
            return Ok(None);
        }
        self.start = if text && self.end == MAX_DATA_SIZE {
            memchr::memrchr(b'\n', &self.data[..self.end]).map_or(self.end, |position| position + 1)
        } else {
            self.end
        };
        Ok(Some(&self.data[..self.start]))
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

fn compress_blocks(level: u8, work_rx: Receiver<Work>, done_tx: &Sender<Done>) {
    let mut compressor = compressor(level);

    while let Ok(work) = work_rx.recv() {
        let mut frame = vec![0; FRAME_SIZE];
        let frame = encode_frame(&work.data, compressor.as_mut(), &mut frame).map(|size| {
            frame.truncate(size);
            frame
        });
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

fn compressor(level: u8) -> Option<Compressor> {
    const EFFORT: [i32; 10] = [0, 1, 2, 3, 5, 6, 7, 8, 10, 12];
    (level != 0).then(|| {
        Compressor::new(
            CompressionLvl::new(EFFORT[usize::from(level)])
                .expect("HTSlib effort mapping uses supported libdeflate levels"),
        )
    })
}

fn encode_frame(
    data: &[u8],
    compressor: Option<&mut Compressor>,
    frame: &mut [u8],
) -> io::Result<usize> {
    let payload = &mut frame[HEADER_SIZE..FRAME_SIZE - TRAILER_SIZE];
    let compressed_size = match compressor {
        Some(compressor) => compressor
            .deflate_compress(data, payload)
            .map_err(|error| io::Error::other(format!("deflate failed: {error:?}")))?,
        None => {
            let length = u16::try_from(data.len())
                .map_err(|_| io::Error::other("stored BGZF block exceeds 64 KiB"))?;
            payload[0] = 0x01;
            payload[1..3].copy_from_slice(&length.to_le_bytes());
            payload[3..5].copy_from_slice(&(!length).to_le_bytes());
            payload[5..5 + data.len()].copy_from_slice(data);
            data.len() + 5
        }
    };

    let block_size = HEADER_SIZE
        .checked_add(compressed_size)
        .and_then(|size| size.checked_add(TRAILER_SIZE))
        .ok_or_else(|| io::Error::other("BGZF block size overflow"))?;
    let bsize = u16::try_from(block_size - 1)
        .map_err(|_| io::Error::other("compressed BGZF block exceeds 64 KiB"))?;

    frame[..16].copy_from_slice(&[
        0x1f, 0x8b, 0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x06, 0x00, b'B', b'C', 0x02,
        0x00,
    ]);
    frame[16..HEADER_SIZE].copy_from_slice(&bsize.to_le_bytes());
    let trailer = HEADER_SIZE + compressed_size;
    frame[trailer..trailer + 4].copy_from_slice(&crc32fast::hash(data).to_le_bytes());
    frame[trailer + 4..block_size].copy_from_slice(&(data.len() as u32).to_le_bytes());
    Ok(block_size)
}

fn write_frames<W: Write>(
    sink: W,
    done_rx: Receiver<Done>,
    credit_tx: Sender<()>,
) -> io::Result<(W, u64, u64)> {
    let mut sink = BufWriter::with_capacity(OUTPUT_BUFFER_SIZE, sink);
    let mut pending = BTreeMap::new();
    let mut next = 0u64;
    let mut bytes_out = 0u64;

    while let Ok(done) = done_rx.recv() {
        pending.insert(done.sequence, done.frame?);
        while let Some(frame) = pending.remove(&next) {
            sink.write_all(&frame)?;
            bytes_out += frame.len() as u64;
            next += 1;
            let _ = credit_tx.send(());
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
    Ok((
        sink.into_inner().map_err(|error| error.into_error())?,
        bytes_out,
        next,
    ))
}
