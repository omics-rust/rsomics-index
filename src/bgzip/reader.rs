use std::io::{self, Read, Write};
use std::ops::Range;

use noodles_bgzf::io::Reader;

use super::StreamStats;

const EOF_BLOCK: [u8; 28] = [
    0x1f, 0x8b, 0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x06, 0x00, b'B', b'C', 0x02, 0x00,
    0x1b, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

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

    let tracked = TailReader::new(source);
    let mut reader = Reader::new(tracked);
    let bytes_out = io::copy(&mut reader, &mut sink)?;
    sink.flush()?;
    let tracked = reader.into_inner();
    if !tracked.has_complete_eof() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "BGZF stream is missing its complete EOF marker",
        ));
    }

    Ok(StreamStats {
        bytes_in: tracked.bytes_read,
        bytes_out,
        blocks: 0,
    })
}

struct TailReader<R> {
    inner: R,
    frames: FrameTracker,
    bytes_read: u64,
}

impl<R> TailReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            frames: FrameTracker::default(),
            bytes_read: 0,
        }
    }

    fn has_complete_eof(&self) -> bool {
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

#[derive(Default)]
struct FrameTracker {
    frame: Vec<u8>,
    header_end: Option<usize>,
    frame_end: Option<usize>,
    saw_eof: bool,
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

fn parse_block_size(header: &[u8]) -> io::Result<usize> {
    let mut position = 12;
    let mut block_size = None;
    while position < header.len() {
        let fields_end = position
            .checked_add(4)
            .ok_or_else(|| invalid("BGZF extra subfield overflow"))?;
        let fields = header
            .get(position..fields_end)
            .ok_or_else(|| invalid("truncated BGZF extra subfield"))?;
        let data_len = usize::from(u16::from_le_bytes([fields[2], fields[3]]));
        let data_end = fields_end
            .checked_add(data_len)
            .ok_or_else(|| invalid("BGZF extra subfield length overflow"))?;
        let data = header
            .get(fields_end..data_end)
            .ok_or_else(|| invalid("truncated BGZF extra subfield data"))?;
        if fields[..2] == *b"BC" {
            if data.len() != 2 || block_size.is_some() {
                return Err(invalid("invalid or duplicate BGZF BC subfield"));
            }
            block_size = Some(usize::from(u16::from_le_bytes([data[0], data[1]])) + 1);
        }
        position = data_end;
    }
    block_size.ok_or_else(|| invalid("BGZF header has no BC subfield"))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
