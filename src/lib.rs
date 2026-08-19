#![deny(missing_docs)]

//! Checked BGZF and tabix workflows used by the `rsomics-index` product.

mod cli;
mod commands;
mod output;

/// BGZF compression, decompression, and GZI workflows.
pub mod bgzip;
/// Tabix configuration, index construction, and query workflows.
pub mod tabix;

#[doc(hidden)]
#[must_use]
pub fn run_binary() -> std::process::ExitCode {
    cli::run()
}
