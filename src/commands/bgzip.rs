use std::fs::File;
use std::io::{self, Read};
use std::num::NonZero;
use std::path::{Path, PathBuf};

use clap::{ArgGroup, Args};
use rsomics_common::{
    AtomicFile, Context, Result, RsomicsError, reject_output_alias, write_atomic, write_output,
};
use serde::Serialize;

use crate::bgzip::{
    CompressOptions, GziIndex, StreamStats, compress, decompress, decompress_indexed,
};

use super::{ensure_replaceable, named_path, require_named_json_output, sidecar_path};

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("mode")
        .args(["decompress", "test", "reindex"])
        .multiple(false)
))]
pub struct Arguments {
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

pub fn execute(arguments: Arguments, json: bool) -> Result<Summary> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Compress,
    Decompress,
    Test,
    Reindex,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub mode: Mode,
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub index_input: Option<PathBuf>,
    pub index_output: Option<PathBuf>,
    pub offset: Option<u64>,
    pub size: Option<u64>,
    pub compression: CompressOptions,
    pub force: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            mode: Mode::Compress,
            input: PathBuf::from("-"),
            output: None,
            index_input: None,
            index_output: None,
            offset: None,
            size: None,
            compression: CompressOptions::default(),
            force: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub mode: Mode,
    pub stream: Option<StreamStats>,
    pub index_entries: Option<u64>,
}

pub fn run(options: &RunOptions) -> Result<Summary> {
    validate(options)?;
    match options.mode {
        Mode::Compress => run_compress(options),
        Mode::Decompress => run_decompress(options),
        Mode::Test => run_test(options),
        Mode::Reindex => run_reindex(options),
    }
}

fn run_compress(options: &RunOptions) -> Result<Summary> {
    let input = open_input(&options.input)?;
    let Some(output) = named_path(options.output.as_deref()) else {
        let (_, stream) = compress(input, io::stdout(), &options.compression)?;
        return Ok(Summary {
            mode: options.mode,
            stream: Some(stream),
            index_entries: None,
        });
    };

    reject_output_alias(output, [options.input.as_path()])?;
    ensure_replaceable(output, options.force)?;
    match options.index_output.as_deref() {
        Some(index_path) => {
            ensure_named(index_path, "index output")?;
            reject_output_alias(index_path, [options.input.as_path(), output])?;
            ensure_replaceable(index_path, options.force)?;

            let data = AtomicFile::new(output)?;
            let mut sidecar = AtomicFile::new(index_path)?;
            let (sink, stream) = compress(input, data.reopen()?, &options.compression)?;
            drop(sink);
            let mut staged = File::open(data.temporary_path()).rs_with_context(|| {
                format!("opening staged BGZF output beside {}", output.display())
            })?;
            let index = GziIndex::scan(&mut staged)?;
            index.write(sidecar.file_mut())?;
            let index_entries = index.entries().len() as u64;
            AtomicFile::commit_all(vec![data, sidecar])?;
            Ok(Summary {
                mode: options.mode,
                stream: Some(stream),
                index_entries: Some(index_entries),
            })
        }
        None => {
            let data = AtomicFile::new(output)?;
            let (sink, stream) = compress(input, data.reopen()?, &options.compression)?;
            drop(sink);
            data.commit()?;
            Ok(Summary {
                mode: options.mode,
                stream: Some(stream),
                index_entries: None,
            })
        }
    }
}

fn run_decompress(options: &RunOptions) -> Result<Summary> {
    prepare_output(options)?;
    let stream = match options.offset {
        Some(offset) => {
            let input_path = named_input(&options.input, "indexed decompression")?;
            let index_path = options
                .index_input
                .clone()
                .unwrap_or_else(|| sidecar_path(input_path, "gzi"));
            ensure_named(&index_path, "index input")?;
            if let Some(output) = named_path(options.output.as_deref()) {
                reject_output_alias(output, [input_path, index_path.as_path()])?;
            }
            let input = File::open(input_path)
                .rs_with_context(|| format!("opening BGZF input {}", input_path.display()))?;
            let index_file = File::open(&index_path)
                .rs_with_context(|| format!("opening GZI index {}", index_path.display()))?;
            let index = GziIndex::read(index_file)
                .rs_with_context(|| format!("reading GZI index {}", index_path.display()))?;
            write_output(options.output.as_deref(), |output| {
                Ok(decompress_indexed(
                    input,
                    &index,
                    offset,
                    options.size,
                    output,
                )?)
            })?
        }
        None => {
            let input = open_input(&options.input)?;
            write_output(options.output.as_deref(), |output| {
                Ok(decompress(input, output, None)?)
            })?
        }
    };

    Ok(Summary {
        mode: options.mode,
        stream: Some(stream),
        index_entries: None,
    })
}

fn run_test(options: &RunOptions) -> Result<Summary> {
    let stream = decompress(open_input(&options.input)?, io::sink(), None)?;
    Ok(Summary {
        mode: options.mode,
        stream: Some(stream),
        index_entries: None,
    })
}

fn run_reindex(options: &RunOptions) -> Result<Summary> {
    let input_path = named_input(&options.input, "reindexing")?;
    let output = options
        .index_output
        .clone()
        .unwrap_or_else(|| sidecar_path(input_path, "gzi"));
    ensure_named(&output, "index output")?;
    reject_output_alias(&output, [input_path])?;
    ensure_replaceable(&output, options.force)?;

    let validation_input = File::open(input_path)
        .rs_with_context(|| format!("opening BGZF input {}", input_path.display()))?;
    let stream = decompress(validation_input, io::sink(), None)
        .rs_with_context(|| format!("validating BGZF input {}", input_path.display()))?;
    let mut input = File::open(input_path)
        .rs_with_context(|| format!("opening BGZF input {}", input_path.display()))?;
    let index = GziIndex::scan(&mut input)
        .rs_with_context(|| format!("scanning BGZF input {}", input_path.display()))?;
    write_atomic(&output, |file| Ok(index.write(file)?))?;

    Ok(Summary {
        mode: options.mode,
        stream: Some(stream),
        index_entries: Some(index.entries().len() as u64),
    })
}

fn validate(options: &RunOptions) -> Result<()> {
    if options.size.is_some() && options.offset.is_none() {
        return Err(config("--size requires --offset"));
    }
    match options.mode {
        Mode::Compress => {
            reject_set(options.offset, "--offset", "compression")?;
            reject_set(options.size, "--size", "compression")?;
            reject_path(
                options.index_input.as_deref(),
                "--index-input",
                "compression",
            )?;
            if options.index_output.is_some() && named_path(options.output.as_deref()).is_none() {
                return Err(config("--index-output requires a named data output"));
            }
        }
        Mode::Decompress => {
            reject_path(
                options.index_output.as_deref(),
                "--index-output",
                "decompression",
            )?;
            if options.index_input.is_some() && options.offset.is_none() {
                return Err(config("--index-input requires --offset"));
            }
        }
        Mode::Test => {
            reject_path(options.output.as_deref(), "--output", "integrity testing")?;
            reject_path(
                options.index_input.as_deref(),
                "--index-input",
                "integrity testing",
            )?;
            reject_path(
                options.index_output.as_deref(),
                "--index-output",
                "integrity testing",
            )?;
            reject_set(options.offset, "--offset", "integrity testing")?;
            reject_set(options.size, "--size", "integrity testing")?;
        }
        Mode::Reindex => {
            reject_path(options.output.as_deref(), "--output", "reindexing")?;
            reject_path(
                options.index_input.as_deref(),
                "--index-input",
                "reindexing",
            )?;
            reject_set(options.offset, "--offset", "reindexing")?;
            reject_set(options.size, "--size", "reindexing")?;
        }
    }
    Ok(())
}

fn prepare_output(options: &RunOptions) -> Result<()> {
    if let Some(output) = named_path(options.output.as_deref()) {
        reject_output_alias(output, [options.input.as_path()])?;
        ensure_replaceable(output, options.force)?;
    }
    Ok(())
}

fn open_input(path: &Path) -> Result<Input> {
    if path == Path::new("-") {
        Ok(Input::Stdin(io::stdin()))
    } else {
        File::open(path)
            .map(Input::File)
            .rs_with_context(|| format!("opening input {}", path.display()))
    }
}

fn named_input<'a>(path: &'a Path, operation: &str) -> Result<&'a Path> {
    if path == Path::new("-") {
        Err(config(format!("{operation} requires a named input")))
    } else {
        Ok(path)
    }
}

fn ensure_named(path: &Path, role: &str) -> Result<()> {
    if path == Path::new("-") {
        Err(config(format!("{role} must be a named file")))
    } else {
        Ok(())
    }
}

fn reject_set<T>(value: Option<T>, flag: &str, operation: &str) -> Result<()> {
    if value.is_some() {
        Err(config(format!("{flag} is not valid for {operation}")))
    } else {
        Ok(())
    }
}

fn reject_path(value: Option<&Path>, flag: &str, operation: &str) -> Result<()> {
    if value.is_some() {
        Err(config(format!("{flag} is not valid for {operation}")))
    } else {
        Ok(())
    }
}

fn config(message: impl Into<String>) -> RsomicsError {
    RsomicsError::ConfigError(message.into())
}

enum Input {
    File(File),
    Stdin(io::Stdin),
}

impl Read for Input {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.read(buffer),
            Self::Stdin(stdin) => stdin.read(buffer),
        }
    }
}
