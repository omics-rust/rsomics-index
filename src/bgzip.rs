use std::io::{self, Read, Write};
use std::num::NonZero;
use std::ops::Range;

pub(crate) mod frame;
mod index;
pub(crate) mod reader;
mod workflow;
mod writer;

pub use index::GziIndex;
pub use workflow::{Mode, RunOptions, Summary, run};

#[derive(Debug, Clone, Copy)]
/// Options controlling BGZF block compression.
pub struct CompressOptions {
    /// Deflate compression level in the inclusive range 0 through 9.
    pub level: u8,
    /// Number of compression workers.
    pub workers: NonZero<usize>,
    /// Prefer newline-aligned block boundaries when possible.
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
/// Byte and block counts for one completed BGZF operation.
pub struct StreamStats {
    /// Bytes consumed from the input stream.
    pub bytes_in: u64,
    /// Bytes written to the output stream.
    pub bytes_out: u64,
    /// Non-EOF BGZF blocks processed.
    pub blocks: u64,
}

/// Compresses a stream into canonical BGZF and returns the finalized sink.
pub fn compress<R, W>(source: R, sink: W, options: &CompressOptions) -> io::Result<(W, StreamStats)>
where
    R: Read,
    W: Write + Send + 'static,
{
    writer::compress(source, sink, options)
}

/// Decompresses a complete BGZF stream or an uncompressed byte range.
pub fn decompress<R, W>(source: R, sink: W, range: Option<Range<u64>>) -> io::Result<StreamStats>
where
    R: Read,
    W: Write,
{
    reader::decompress(source, sink, range)
}

/// Decompresses an uncompressed byte range using a GZI sidecar.
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
