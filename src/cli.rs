use std::process;

use clap::{Parser, Subcommand};
use rsomics_common::{OutputArgs, Result, ToolMeta, run as run_tool};
use serde::Serialize;

use crate::commands::{bgzip, tabix};

const META: ToolMeta = ToolMeta {
    name: "rsomics-index",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Parser)]
#[command(
    name = "rsomics-index",
    version,
    about = "Prepare and query indexed genomic resources",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(flatten)]
    output: OutputArgs,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compress, decompress, validate, or index BGZF streams
    Bgzip(bgzip::Arguments),
    /// Build and query local TBI and CSI indexes
    Tabix(tabix::Arguments),
}

#[derive(Debug, Serialize)]
#[serde(tag = "command", content = "report", rename_all = "snake_case")]
enum CommandReport {
    Bgzip(bgzip::Summary),
    Tabix(tabix::Report),
}

#[must_use]
pub(crate) fn run() -> process::ExitCode {
    let cli = rsomics_help::parse::<Cli>();
    let output = cli.output.clone();
    run_tool(&output, META, || execute(cli))
}

fn execute(cli: Cli) -> Result<CommandReport> {
    match cli.command {
        Command::Bgzip(arguments) => {
            bgzip::execute(arguments, cli.output.json).map(CommandReport::Bgzip)
        }
        Command::Tabix(arguments) => {
            tabix::execute(arguments, cli.output.json).map(CommandReport::Tabix)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn command_tree_is_valid() {
        rsomics_help::command::<Cli>().debug_assert();
        Cli::command().debug_assert();
    }
}
