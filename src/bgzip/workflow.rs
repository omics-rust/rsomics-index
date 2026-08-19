use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rsomics_common::{
    AtomicFile, Context, Result, RsomicsError, reject_output_alias, write_atomic, write_output,
};
use serde::Serialize;

use crate::output::{ensure_replaceable, named_path, sidecar_path};

use super::{CompressOptions, GziIndex, StreamStats, compress, decompress, decompress_indexed};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// BGZF workflow selected by [`run`].
pub enum Mode {
    /// Compress plain input into BGZF.
    Compress,
    /// Decompress BGZF input.
    Decompress,
    /// Validate BGZF input without retaining decoded bytes.
    Test,
    /// Rebuild a GZI sidecar for BGZF input.
    Reindex,
}

#[derive(Debug, Clone)]
/// Path, range, compression, and replacement policy for [`run`].
pub struct RunOptions {
    /// Operation to perform.
    pub mode: Mode,
    /// Input path, or `-` for standard input where supported.
    pub input: PathBuf,
    /// Data output path, or `-` for standard output.
    pub output: Option<PathBuf>,
    /// Existing GZI sidecar used for indexed decompression.
    pub index_input: Option<PathBuf>,
    /// GZI sidecar created during compression or reindexing.
    pub index_output: Option<PathBuf>,
    /// Zero-based uncompressed offset for indexed decompression.
    pub offset: Option<u64>,
    /// Maximum uncompressed bytes to emit after `offset`.
    pub size: Option<u64>,
    /// Compression settings used in [`Mode::Compress`].
    pub compression: CompressOptions,
    /// Whether existing named outputs may be replaced.
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
/// Summary of a completed BGZF workflow.
pub struct Summary {
    /// Operation that completed.
    pub mode: Mode,
    /// Stream counts when the operation processed BGZF data.
    pub stream: Option<StreamStats>,
    /// GZI entry count when the operation created an index.
    pub index_entries: Option<u64>,
}

/// Runs one BGZF file workflow with transactional named outputs.
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
