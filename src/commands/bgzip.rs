use std::num::NonZero;
use std::path::PathBuf;

use clap::{ArgGroup, Args};
use rsomics_common::{Result, RsomicsError};

use crate::bgzip::{CompressOptions, Mode, RunOptions, Summary, run};

use super::require_named_json_output;

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("mode")
        .args(["decompress", "test", "reindex"])
        .multiple(false)
))]
pub(crate) struct Arguments {
    /// Input file; use - or omit for standard input
    #[arg(value_name = "INPUT", default_value = "-")]
    input: PathBuf,

    /// Write data to this file; omit or use - for standard output
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Decompress BGZF input
    #[arg(short, long)]
    decompress: bool,

    /// Validate BGZF input without writing decompressed data
    #[arg(short = 't', long)]
    test: bool,

    /// Rebuild a GZI sidecar for an existing BGZF file
    #[arg(short = 'r', long)]
    reindex: bool,

    /// Deflate compression level
    #[arg(short = 'l', long = "compress-level", value_name = "0..9", value_parser = clap::value_parser!(u8).range(0..=9))]
    level: Option<u8>,

    /// Compression worker count
    #[arg(short = '@', long, value_name = "N")]
    threads: Option<NonZero<usize>>,

    /// Fill blocks without preserving newline boundaries
    #[arg(long)]
    binary: bool,

    /// GZI sidecar used for indexed decompression
    #[arg(long, value_name = "GZI")]
    index_input: Option<PathBuf>,

    /// GZI sidecar created during compression or reindexing
    #[arg(long, value_name = "GZI")]
    index_output: Option<PathBuf>,

    /// Begin indexed decompression at this uncompressed offset
    #[arg(short = 'b', long, value_name = "OFFSET")]
    offset: Option<u64>,

    /// Emit at most this many uncompressed bytes
    #[arg(short = 's', long, value_name = "BYTES")]
    size: Option<u64>,

    /// Replace existing named outputs
    #[arg(short, long)]
    force: bool,
}

pub(crate) fn execute(arguments: Arguments, json: bool) -> Result<Summary> {
    let mode = if arguments.decompress {
        Mode::Decompress
    } else if arguments.test {
        Mode::Test
    } else if arguments.reindex {
        Mode::Reindex
    } else {
        Mode::Compress
    };
    if mode != Mode::Compress
        && (arguments.level.is_some() || arguments.threads.is_some() || arguments.binary)
    {
        return Err(config(
            "compression level, threads, and --binary are valid only for compression",
        ));
    }
    if matches!(mode, Mode::Compress | Mode::Decompress) {
        require_named_json_output(json, arguments.output.as_deref(), "BGZF data")?;
    }
    run(&RunOptions {
        mode,
        input: arguments.input,
        output: arguments.output,
        index_input: arguments.index_input,
        index_output: arguments.index_output,
        offset: arguments.offset,
        size: arguments.size,
        compression: CompressOptions {
            level: arguments.level.unwrap_or(6),
            workers: arguments.threads.unwrap_or(NonZero::<usize>::MIN),
            text: !arguments.binary,
        },
        force: arguments.force,
    })
}

fn config(message: impl Into<String>) -> RsomicsError {
    RsomicsError::ConfigError(message.into())
}
