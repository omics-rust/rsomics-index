mod cli;

pub mod bgzip;
pub mod commands;
pub mod tabix;

#[doc(hidden)]
#[must_use]
pub fn run_binary() -> std::process::ExitCode {
    cli::run()
}
