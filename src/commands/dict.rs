use std::path::PathBuf;

use clap::Args;
use rsomics_common::Result;

use crate::dict::{Options, Summary, run};

use super::require_named_json_output;

#[derive(Debug, Args)]
pub(crate) struct Arguments {
    /// Plain, gzip, or BGZF FASTA; use - for standard input
    input: PathBuf,
    /// SAM dictionary destination; omit or use - for standard output
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
    /// Explicit UR tag; default is the canonical file URI, absent for stdin
    #[arg(short = 'u', long, value_name = "URI")]
    uri: Option<String>,
    /// Assembly AS tag
    #[arg(short = 'a', long, value_name = "NAME")]
    assembly: Option<String>,
    /// Species SP tag
    #[arg(short = 's', long, value_name = "NAME")]
    species: Option<String>,
    /// Add chr and mitochondrial AN aliases
    #[arg(short = 'A', long)]
    alias: bool,
    /// Omit the HD line
    #[arg(short = 'H', long)]
    no_header: bool,
    /// Replace an existing named output
    #[arg(short, long)]
    force: bool,
}

pub(crate) fn execute(arguments: Arguments, json: bool) -> Result<Summary> {
    require_named_json_output(json, arguments.output.as_deref(), "SAM dictionary data")?;
    run(&Options {
        input: arguments.input,
        output: arguments.output,
        uri: arguments.uri,
        assembly: arguments.assembly,
        species: arguments.species,
        alias: arguments.alias,
        no_header: arguments.no_header,
        force: arguments.force,
    })
}
