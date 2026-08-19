pub mod bgzip;
pub mod tabix;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rsomics_common::{Result, RsomicsError};

pub(super) fn named_path(path: Option<&Path>) -> Option<&Path> {
    path.filter(|path| *path != Path::new("-"))
}

pub(super) fn require_named_json_output(
    json: bool,
    path: Option<&Path>,
    description: &str,
) -> Result<()> {
    if json && named_path(path).is_none() {
        Err(RsomicsError::ConfigError(format!(
            "--json requires a named --output for {description}"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn ensure_replaceable(path: &Path, force: bool) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => Err(RsomicsError::InvalidInput(format!(
            "output {} is not a regular file",
            path.display()
        ))),
        Ok(_) if !force => Err(RsomicsError::ConfigError(format!(
            "output {} already exists; use --force to replace it",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RsomicsError::Io(io::Error::new(
            error.kind(),
            format!("inspecting output {}: {error}", path.display()),
        ))),
    }
}

pub(super) fn sidecar_path(path: &Path, extension: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".");
    value.push(extension);
    PathBuf::from(value)
}
