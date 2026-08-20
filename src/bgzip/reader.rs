use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;

use libdeflater::Decompressor;

use super::{
    GziIndex, StreamStats,
    frame::{
        EOF_BLOCK, decode_frame, frame_uncompressed_size, invalid, parse_block_size, read_frame,
    },
};

pub(super) fn decompress<R, W>(
    source: R,
    mut sink: W,
    range: Option<Range<u64>>,
) -> io::Result<StreamStats>
where
    R: Read,
    W: Write,
{
    if range.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "indexed BGZF ranges require a GZI index",
        ));
    }

    decode_stream(source, &mut sink, 0, None)
}

pub(super) fn decompress_indexed<R, W>(
    mut source: R,
    index: &GziIndex,
    offset: u64,
    size: Option<u64>,
    mut sink: W,
) -> io::Result<StreamStats>
where
    R: Read + Seek,
    W: Write,
{
    let position = index.query(offset)?;
    source.seek(SeekFrom::Start(position.compressed()))?;
    decode_stream(
        source,
        &mut sink,
        usize::from(position.uncompressed()),
        size,
    )
}

fn decode_stream<R, W>(
    mut source: R,
    mut sink: W,
    mut skip: usize,
    mut remaining: Option<u64>,
) -> io::Result<StreamStats>
where
    R: Read,
    W: Write,
{
    let mut decompressor = Decompressor::new();
    let mut bytes_in = 0u64;
    let mut bytes_out = 0u64;
    let mut blocks = 0u64;

    while remaining != Some(0) {
        let frame = read_frame(&mut source).map_err(normalize_truncation)?;
        bytes_in = bytes_in
            .checked_add(frame.len() as u64)
            .ok_or_else(|| io::Error::other("compressed byte count overflow"))?;
        if frame == EOF_BLOCK {
            let mut trailing = [0];
            if source.read(&mut trailing)? != 0 {
                return Err(invalid("bytes found after the BGZF EOF marker"));
            }
            sink.flush()?;
            return Ok(StreamStats {
                bytes_in,
                bytes_out,
                blocks,
            });
        }

        let data = decode_frame(&mut decompressor, &frame)?;
        if skip > data.len() {
            return Err(invalid("indexed offset exceeds its BGZF block"));
        }
        let available = &data[skip..];
        skip = 0;
        let available_bytes = available.len() as u64;
        let size = usize::try_from(remaining.unwrap_or(available_bytes).min(available_bytes))
            .map_err(|_| io::Error::other("BGZF output size exceeds usize"))?;
        sink.write_all(&available[..size])?;
        bytes_out = bytes_out
            .checked_add(size as u64)
            .ok_or_else(|| io::Error::other("uncompressed byte count overflow"))?;
        blocks = blocks
            .checked_add(1)
            .ok_or_else(|| io::Error::other("BGZF block count overflow"))?;
        if let Some(limit) = &mut remaining {
            *limit -= size as u64;
        }
    }

    sink.flush()?;
    Ok(StreamStats {
        bytes_in,
        bytes_out,
        blocks,
    })
}

pub(crate) struct TailReader<R> {
    inner: R,
    frames: FrameTracker,
    bytes_read: u64,
}

impl<R> TailReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            frames: FrameTracker::default(),
            bytes_read: 0,
        }
    }

    pub(crate) fn has_complete_eof(&self) -> bool {
        self.frames.saw_eof && self.frames.frame.is_empty()
    }
}

impl<R: Read> Read for TailReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let size = self.inner.read(buffer)?;
        self.bytes_read = self
            .bytes_read
            .checked_add(size as u64)
            .ok_or_else(|| io::Error::other("compressed byte count overflow"))?;
        self.frames.observe(&buffer[..size])?;
        Ok(size)
    }
}

impl<R: Seek> Seek for TailReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = self.inner.seek(position)?;
        self.frames = FrameTracker::default();
        self.bytes_read = 0;
        Ok(position)
    }
}

#[derive(Default)]
struct FrameTracker {
    frame: Vec<u8>,
    header_end: Option<usize>,
    frame_end: Option<usize>,
    saw_eof: bool,
    data_blocks: u64,
}

impl FrameTracker {
    fn observe(&mut self, bytes: &[u8]) -> io::Result<()> {
        for byte in bytes {
            if self.saw_eof {
                return Err(invalid("bytes found after the BGZF EOF marker"));
            }
            self.frame.push(*byte);
            self.update_bounds()?;
            if self.frame_end == Some(self.frame.len()) {
                if self.frame == EOF_BLOCK {
                    self.saw_eof = true;
                } else {
                    if frame_uncompressed_size(&self.frame)? == 0 {
                        return Err(invalid("noncanonical empty BGZF block"));
                    }
                    self.data_blocks = self
                        .data_blocks
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("BGZF block count overflow"))?;
                }
                self.frame.clear();
                self.header_end = None;
                self.frame_end = None;
            }
        }
        Ok(())
    }

    fn update_bounds(&mut self) -> io::Result<()> {
        if self.frame.len() == 4 && self.frame[..4] != [0x1f, 0x8b, 0x08, 0x04] {
            return Err(invalid("invalid BGZF gzip header"));
        }

        if self.frame.len() == 12 {
            let extra_len = usize::from(u16::from_le_bytes([self.frame[10], self.frame[11]]));
            self.header_end = Some(
                12usize
                    .checked_add(extra_len)
                    .ok_or_else(|| invalid("BGZF extra-field length overflow"))?,
            );
        }

        if self.header_end == Some(self.frame.len()) {
            let block_size = parse_block_size(&self.frame)?;
            if block_size < self.frame.len() + 8 {
                return Err(invalid("BGZF block is shorter than its header and trailer"));
            }
            self.frame_end = Some(block_size);
        }

        if self.frame_end.is_some_and(|end| self.frame.len() > end) {
            return Err(invalid("BGZF block exceeds its declared size"));
        }
        Ok(())
    }
}

pub(crate) fn normalize_truncation(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "BGZF stream ended before its EOF marker",
        )
    } else {
        error
    }
}
