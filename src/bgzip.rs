use std::io::{self, Read, Write};
use std::num::NonZero;
use std::ops::Range;

mod reader;
mod writer;

#[derive(Debug, Clone, Copy)]
pub struct CompressOptions {
    pub level: u8,
    pub workers: NonZero<usize>,
    pub text: bool,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            level: 6,
            workers: NonZero::<usize>::MIN,
            text: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamStats {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub blocks: u64,
}

pub fn compress<R, W>(source: R, sink: W, options: &CompressOptions) -> io::Result<(W, StreamStats)>
where
    R: Read,
    W: Write + Send + 'static,
{
    writer::compress(source, sink, options)
}

pub fn decompress<R, W>(source: R, sink: W, range: Option<Range<u64>>) -> io::Result<StreamStats>
where
    R: Read,
    W: Write,
{
    reader::decompress(source, sink, range)
}
