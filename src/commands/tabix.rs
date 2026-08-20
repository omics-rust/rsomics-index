use std::fs::File;
use std::io::Read;
use std::num::NonZero;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use rsomics_common::{Context, Result, RsomicsError, reject_output_alias, write_output};
use serde::Serialize;

use crate::output::{ensure_replaceable, named_path, sidecar_path};
use crate::tabix::{
    BuildOptions, BuildSummary, Config, CoordinateSystem, IndexKind, ListSummary, Preset,
    QueryOptions, QuerySummary, build_named, list, query,
};

use super::require_named_json_output;

const DEFAULT_CACHE_BYTES: usize = 10 * 1024 * 1024;
const DETECTION_SAMPLE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Args)]
pub(crate) struct Arguments {
    #[command(subcommand)]
    operation: Operation,
}

#[derive(Debug, Subcommand)]
enum Operation {
    /// Build a TBI or CSI index for sorted BGZF data
    Build(BuildArguments),
    /// Query records by region or target intervals
    Query(QueryArguments),
    /// List indexed reference names in stored order
    List(ListArguments),
}

#[derive(Debug, Args)]
struct BuildArguments {
    /// Coordinate-sorted BGZF input
    #[arg(value_name = "DATA")]
    input: PathBuf,

    /// Index output; defaults to DATA.tbi or DATA.csi
    #[arg(short, long, value_name = "INDEX")]
    output: Option<PathBuf>,

    /// Input format preset
    #[arg(short, long, value_enum)]
    preset: Option<PresetArgument>,

    /// One-based reference-name column
    #[arg(short = 's', long, value_name = "COLUMN")]
    sequence_column: Option<usize>,

    /// One-based start-coordinate column
    #[arg(short = 'b', long, value_name = "COLUMN")]
    begin_column: Option<usize>,

    /// One-based end-coordinate column
    #[arg(short = 'e', long, value_name = "COLUMN")]
    end_column: Option<usize>,

    /// Treat custom columns as zero-based half-open coordinates
    #[arg(short = '0', long)]
    zero_based: bool,

    /// Single-byte header and comment prefix
    #[arg(short = 'c', long, value_name = "BYTE", value_parser = parse_comment)]
    comment: Option<u8>,

    /// Skip this many leading lines
    #[arg(short = 'S', long, value_name = "N")]
    skip_lines: Option<u64>,

    /// Build CSI rather than TBI
    #[arg(short = 'C', long)]
    csi: bool,

    /// CSI minimum interval shift
    #[arg(short = 'm', long, value_name = "1..31", requires = "csi", value_parser = clap::value_parser!(u8).range(1..=31))]
    min_shift: Option<u8>,

    /// Replace an existing index
    #[arg(short, long)]
    force: bool,
}

#[derive(Debug, Args)]
struct QueryArguments {
    /// BGZF data file
    #[arg(value_name = "DATA")]
    input: PathBuf,

    /// One-based inclusive region; repeat for ordered queries
    #[arg(value_name = "REGION")]
    regions: Vec<String>,

    /// Explicit TBI or CSI index
    #[arg(short, long, value_name = "INDEX")]
    index: Option<PathBuf>,

    /// BED or one-based tabular region file
    #[arg(short = 'R', long, value_name = "FILE")]
    regions_file: Option<PathBuf>,

    /// BED or one-based tabular target file
    #[arg(short = 'T', long, value_name = "FILE")]
    targets_file: Option<PathBuf>,

    /// Include leading header lines
    #[arg(long, conflicts_with = "header_only")]
    print_header: bool,

    /// Emit only leading header lines
    #[arg(long)]
    header_only: bool,

    /// Emit each physical record at most once
    #[arg(long, conflicts_with = "separate_regions")]
    unique: bool,

    /// Prefix each region result with a region marker
    #[arg(long)]
    separate_regions: bool,

    /// BGZF decoding worker count
    #[arg(short = '@', long, value_name = "N", default_value = "1")]
    threads: NonZero<usize>,

    /// Maximum bytes retained in the decompressed-block cache
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_CACHE_BYTES)]
    cache_bytes: usize,

    /// Query output; omit or use - for standard output
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Replace an existing named output
    #[arg(short, long)]
    force: bool,
}

#[derive(Debug, Args)]
struct ListArguments {
    /// BGZF data file
    #[arg(value_name = "DATA")]
    input: PathBuf,

    /// Explicit TBI or CSI index
    #[arg(short, long, value_name = "INDEX")]
    index: Option<PathBuf>,

    /// Reference-name output; omit or use - for standard output
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Replace an existing named output
    #[arg(short, long)]
    force: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PresetArgument {
    Bed,
    Gff,
    Sam,
    Vcf,
}

#[derive(Debug, Serialize)]
#[serde(tag = "operation", content = "summary", rename_all = "snake_case")]
pub(crate) enum Report {
    Build(BuildSummary),
    Query(QuerySummary),
    List(ListSummary),
}

pub(crate) fn execute(arguments: Arguments, json: bool) -> Result<Report> {
    match arguments.operation {
        Operation::Build(arguments) => run_build(arguments).map(Report::Build),
        Operation::Query(arguments) => run_query(arguments, json).map(Report::Query),
        Operation::List(arguments) => run_list(arguments, json).map(Report::List),
    }
}

fn run_build(arguments: BuildArguments) -> Result<BuildSummary> {
    let kind = if arguments.csi {
        IndexKind::Csi {
            min_shift: arguments.min_shift.unwrap_or(14),
        }
    } else {
        IndexKind::Tbi
    };
    let output = arguments.output.clone().unwrap_or_else(|| {
        sidecar_path(
            &arguments.input,
            match kind {
                IndexKind::Tbi => "tbi",
                IndexKind::Csi { .. } => "csi",
            },
        )
    });
    if output == Path::new("-") {
        return Err(config("tabix indexes require a named --output"));
    }
    reject_output_alias(&output, [arguments.input.as_path()])?;
    ensure_replaceable(&output, arguments.force)?;
    let config = build_config(&arguments)?;
    build_named(&arguments.input, &output, &BuildOptions { config, kind })
}

fn run_query(arguments: QueryArguments, json: bool) -> Result<QuerySummary> {
    require_named_json_output(json, arguments.output.as_deref(), "query data")?;
    prepare_stream_output(
        &arguments.input,
        arguments.index.as_deref(),
        arguments.regions_file.as_deref(),
        arguments.targets_file.as_deref(),
        arguments.output.as_deref(),
        arguments.force,
    )?;
    let options = QueryOptions {
        regions: arguments.regions,
        regions_file: arguments.regions_file,
        targets_file: arguments.targets_file,
        index: arguments.index,
        print_header: arguments.print_header,
        header_only: arguments.header_only,
        unique: arguments.unique,
        separate_regions: arguments.separate_regions,
        workers: arguments.threads,
        cache_bytes: arguments.cache_bytes,
    };
    write_output(arguments.output.as_deref(), |output| {
        query(&arguments.input, output, &options)
    })
}

fn run_list(arguments: ListArguments, json: bool) -> Result<ListSummary> {
    require_named_json_output(json, arguments.output.as_deref(), "reference names")?;
    prepare_stream_output(
        &arguments.input,
        arguments.index.as_deref(),
        None,
        None,
        arguments.output.as_deref(),
        arguments.force,
    )?;
    write_output(arguments.output.as_deref(), |output| {
        list(&arguments.input, output, arguments.index.as_deref())
    })
}

fn build_config(arguments: &BuildArguments) -> Result<Config> {
    let custom = arguments.sequence_column.is_some()
        || arguments.begin_column.is_some()
        || arguments.end_column.is_some()
        || arguments.zero_based
        || arguments.comment.is_some()
        || arguments.skip_lines.is_some();
    if arguments.preset.is_some() && custom {
        return Err(config(
            "--preset cannot be combined with custom column or coordinate options",
        ));
    }
    if custom {
        return Config::custom(
            arguments.sequence_column.unwrap_or(1),
            arguments.begin_column.unwrap_or(4),
            Some(arguments.end_column.unwrap_or(5)),
            if arguments.zero_based {
                CoordinateSystem::ZeroBasedHalfOpen
            } else {
                CoordinateSystem::OneBasedInclusive
            },
            arguments.comment.unwrap_or(b'#'),
            arguments.skip_lines.unwrap_or(0),
        );
    }
    match arguments.preset {
        Some(preset) => Ok(Config::from_preset(preset.into())),
        None => detect_config(&arguments.input),
    }
}

fn detect_config(input: &Path) -> Result<Config> {
    let file =
        File::open(input).rs_with_context(|| format!("opening BGZF input {}", input.display()))?;
    let mut reader = noodles_bgzf::io::Reader::new(file).take(DETECTION_SAMPLE_BYTES);
    let mut sample = Vec::new();
    reader
        .read_to_end(&mut sample)
        .rs_with_context(|| format!("sampling BGZF input {}", input.display()))?;
    if sample.len() == DETECTION_SAMPLE_BYTES as usize
        && sample.last() != Some(&b'\n')
        && let Some(last_newline) = sample.iter().rposition(|byte| *byte == b'\n')
    {
        sample.truncate(last_newline + 1);
    }
    Config::detect(Some(input), &sample)
}

fn prepare_stream_output(
    input: &Path,
    index: Option<&Path>,
    regions: Option<&Path>,
    targets: Option<&Path>,
    output: Option<&Path>,
    force: bool,
) -> Result<()> {
    let Some(output) = named_path(output) else {
        return Ok(());
    };
    let mut inputs = vec![input.to_owned()];
    match index {
        Some(index) => inputs.push(index.to_owned()),
        None => {
            inputs.push(sidecar_path(input, "tbi"));
            inputs.push(sidecar_path(input, "csi"));
        }
    }
    inputs.extend(regions.map(Path::to_owned));
    inputs.extend(targets.map(Path::to_owned));
    reject_output_alias(output, inputs.iter().map(PathBuf::as_path))?;
    ensure_replaceable(output, force)
}

impl From<PresetArgument> for Preset {
    fn from(value: PresetArgument) -> Self {
        match value {
            PresetArgument::Bed => Self::Bed,
            PresetArgument::Gff => Self::Gff,
            PresetArgument::Sam => Self::Sam,
            PresetArgument::Vcf => Self::Vcf,
        }
    }
}

fn parse_comment(value: &str) -> std::result::Result<u8, String> {
    let bytes = value.as_bytes();
    if bytes.len() == 1 {
        Ok(bytes[0])
    } else {
        Err("comment prefix must be exactly one byte".to_owned())
    }
}

fn config(message: impl Into<String>) -> RsomicsError {
    RsomicsError::ConfigError(message.into())
}
