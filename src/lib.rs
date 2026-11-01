#![deny(missing_docs)]

//! Checked local BGZF, tabix, and reference-resource workflows.

mod cli;
mod commands;
mod dict;
mod faidx;
mod output;
mod reference;

/// BGZF compression, decompression, and GZI workflows.
pub mod bgzip;
/// Tabix configuration, index construction, and query workflows.
pub mod tabix;

#[doc(hidden)]
#[must_use]
pub fn run_binary() -> std::process::ExitCode {
    cli::run()
}
