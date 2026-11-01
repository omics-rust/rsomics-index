use std::path::PathBuf;

use clap::{Args, Subcommand};
use rsomics_common::{Result, ThreadArgs};
use serde::Serialize;

use crate::faidx;

use super::require_named_json_output;

#[derive(Debug, Args)]
pub(crate) struct Arguments {
    #[command(subcommand)]
    operation: Operation,
}

#[derive(Debug, Subcommand)]
enum Operation {
    /// Build FAI and, for BGZF input, GZI sidecars
    Build(Build),
    /// Retrieve regions from an already indexed reference
    Query(Query),
}

#[derive(Debug, Args)]
struct Build {
    /// Plain or BGZF FASTA reference
    reference: PathBuf,
    /// FAI destination; defaults to REFERENCE.fai
    #[arg(long, value_name = "FAI")]
    fai_idx: Option<PathBuf>,
    /// BGZF GZI destination; defaults to REFERENCE.gzi
    #[arg(long, value_name = "GZI")]
    gzi_idx: Option<PathBuf>,
    /// Replace existing named indexes
    #[arg(short, long)]
    force: bool,
}

#[derive(Debug, Args)]
struct Query {
    /// Plain or BGZF FASTA reference
    reference: PathBuf,
    /// One-based inclusive regions, whole names, or braced names
    regions: Vec<String>,
    /// Existing FAI; defaults to REFERENCE.fai
    #[arg(long, value_name = "FAI")]
    fai_idx: Option<PathBuf>,
    /// Existing BGZF GZI; defaults to REFERENCE.gzi
    #[arg(long, value_name = "GZI")]
    gzi_idx: Option<PathBuf>,
    /// One region per line, processed before positional regions
    #[arg(short = 'r', long, value_name = "FILE")]
    region_file: Option<PathBuf>,
    /// Output line length; default preserves input wrapping, zero disables wrapping
    #[arg(short = 'n', long, value_name = "BASES")]
    length: Option<u64>,
    /// FASTA destination; omit or use - for standard output
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
    #[command(flatten)]
    threads: ThreadArgs,
    /// Replace an existing named output
    #[arg(short, long)]
    force: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "operation", content = "summary", rename_all = "snake_case")]
pub(crate) enum Report {
    Build(faidx::BuildSummary),
    Query(faidx::QuerySummary),
}

pub(crate) fn execute(arguments: Arguments, json: bool) -> Result<Report> {
    match arguments.operation {
        Operation::Build(arguments) => faidx::build(
            &arguments.reference,
            arguments.fai_idx.as_deref(),
            arguments.gzi_idx.as_deref(),
            arguments.force,
        )
        .map(Report::Build),
        Operation::Query(arguments) => {
            require_named_json_output(json, arguments.output.as_deref(), "FASTA sequence data")?;
            faidx::query(faidx::QueryOptions {
                reference: arguments.reference,
                fai: arguments.fai_idx,
                gzi: arguments.gzi_idx,
                regions: arguments.regions,
                region_file: arguments.region_file,
                output: arguments.output,
                width: arguments.length,
                workers: arguments.threads.requested().map_or(1, |count| count.get()),
                force: arguments.force,
            })
            .map(Report::Query)
        }
    }
}
