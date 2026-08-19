use std::io::{self, Read, Write};
use std::num::NonZero;
use std::ops::Range;

mod frame;
mod index;
mod reader;
mod writer;

pub use index::GziIndex;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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

pub fn decompress_indexed<R, W>(
    source: R,
    index: &GziIndex,
    offset: u64,
    size: Option<u64>,
    sink: W,
) -> io::Result<StreamStats>
where
    R: Read + std::io::Seek,
    W: Write,
{
    reader::decompress_indexed(source, index, offset, size, sink)
}
