mod config;
mod record;

pub use config::{Config, CoordinateSystem, Preset};
pub use record::{Record, SortedState};

fn trim_line_end(mut line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\n') {
        line = &line[..line.len() - 1];
    }
    if line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    line
}
