pub(crate) mod bgzip;
pub(crate) mod tabix;

use std::path::Path;

use rsomics_common::{Result, RsomicsError};

use crate::output::named_path;

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
