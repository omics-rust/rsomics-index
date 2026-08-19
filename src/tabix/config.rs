use std::num::NonZero;
use std::path::Path;

use rsomics_common::{Context, Result, RsomicsError};

use super::{Record, record, trim_line_end};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Bed,
    Gff,
    Sam,
    Vcf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinateSystem {
    OneBasedInclusive,
    ZeroBasedHalfOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub(super) preset: Option<Preset>,
    pub(super) sequence: NonZero<usize>,
    pub(super) begin: NonZero<usize>,
    pub(super) end: Option<NonZero<usize>>,
    pub(super) coordinates: CoordinateSystem,
    pub(super) comment: u8,
    pub(super) skip: u64,
}

impl Config {
    pub fn from_preset(preset: Preset) -> Self {
        let (sequence, begin, end, coordinates, comment) = match preset {
            Preset::Bed => (1, 2, Some(3), CoordinateSystem::ZeroBasedHalfOpen, b'#'),
            Preset::Gff => (1, 4, Some(5), CoordinateSystem::OneBasedInclusive, b'#'),
            Preset::Sam => (3, 4, None, CoordinateSystem::OneBasedInclusive, b'@'),
            Preset::Vcf => (1, 2, None, CoordinateSystem::OneBasedInclusive, b'#'),
        };
        Self {
            preset: Some(preset),
            sequence: NonZero::new(sequence).expect("preset column is nonzero"),
            begin: NonZero::new(begin).expect("preset column is nonzero"),
            end: end.map(|column| NonZero::new(column).expect("preset column is nonzero")),
            coordinates,
            comment,
            skip: 0,
        }
    }

    pub fn custom(
        sequence: usize,
        begin: usize,
        end: Option<usize>,
        coordinates: CoordinateSystem,
        comment: u8,
        skip: u64,
    ) -> Result<Self> {
        let sequence = NonZero::new(sequence)
            .ok_or_else(|| invalid("sequence column must be greater than zero"))?;
        let begin =
            NonZero::new(begin).ok_or_else(|| invalid("begin column must be greater than zero"))?;
        let end = end
            .map(|column| {
                NonZero::new(column).ok_or_else(|| invalid("end column must be greater than zero"))
            })
            .transpose()?;
        if matches!(comment, 0 | b'\t' | b'\n' | b'\r') {
            return Err(invalid("comment marker is not a valid line prefix"));
        }
        Ok(Self {
            preset: None,
            sequence,
            begin,
            end,
            coordinates,
            comment,
            skip,
        })
    }

    pub fn detect(path: Option<&Path>, sample: &[u8]) -> Result<Self> {
        let mut header = None;
        let mut data = None;
        for line in sample.split(|byte| *byte == b'\n') {
            let line = trim_line_end(line);
            if line.is_empty() {
                continue;
            }
            if let Some(preset) = header_preset(line) {
                merge_hint(&mut header, preset, "headers")?;
                continue;
            }
            if line.starts_with(b"#") {
                continue;
            }
            data.get_or_insert(line);
        }

        let extension = path.and_then(extension_preset);
        if let (Some(header), Some(extension)) = (header, extension)
            && header != extension
        {
            return Err(invalid(format!(
                "format header and file extension conflict ({header:?} vs {extension:?})"
            )));
        }

        if let Some(preset) = header.or(extension) {
            let config = Self::from_preset(preset);
            if let Some(line) = data {
                config
                    .parse(line, 1)
                    .rs_with_context(|| format!("validating detected {preset:?} data"))?;
            }
            return Ok(config);
        }

        let Some(line) = data else {
            return Err(invalid(
                "format cannot be detected from empty or generic header-only input; use --preset",
            ));
        };
        let mut candidates = [Preset::Bed, Preset::Gff, Preset::Sam, Preset::Vcf]
            .into_iter()
            .filter(|preset| Self::from_preset(*preset).parse(line, 1).is_ok());
        let Some(preset) = candidates.next() else {
            return Err(invalid("data does not match a tabix preset; use --preset"));
        };
        if candidates.next().is_some() {
            return Err(invalid("data matches multiple tabix presets; use --preset"));
        }
        Ok(Self::from_preset(preset))
    }

    pub fn parse<'a>(&self, line: &'a [u8], line_no: u64) -> Result<Record<'a>> {
        record::parse(self, line, line_no)
    }

    pub fn is_meta(&self, line_no: u64, line: &[u8]) -> bool {
        line_no <= self.skip || trim_line_end(line).first() == Some(&self.comment)
    }

    pub fn preset(&self) -> Option<Preset> {
        self.preset
    }

    pub fn sequence_column(&self) -> usize {
        self.sequence.get()
    }

    pub fn begin_column(&self) -> usize {
        self.begin.get()
    }

    pub fn end_column(&self) -> Option<usize> {
        self.end.map(NonZero::get)
    }

    pub fn coordinate_system(&self) -> CoordinateSystem {
        self.coordinates
    }

    pub fn comment(&self) -> u8 {
        self.comment
    }

    pub fn skip(&self) -> u64 {
        self.skip
    }

    pub(super) fn max_column(&self) -> usize {
        let formatted = match self.preset {
            Some(Preset::Sam) => 6,
            Some(Preset::Vcf) => 8,
            _ => 0,
        };
        self.sequence
            .get()
            .max(self.begin.get())
            .max(self.end.map_or(0, NonZero::get))
            .max(formatted)
    }
}

fn header_preset(line: &[u8]) -> Option<Preset> {
    if line.starts_with(b"##fileformat=VCF") || line.starts_with(b"#CHROM\tPOS\t") {
        Some(Preset::Vcf)
    } else if line.starts_with(b"##gff-version") {
        Some(Preset::Gff)
    } else if [b"@HD\t", b"@SQ\t", b"@RG\t", b"@PG\t", b"@CO\t"]
        .iter()
        .any(|prefix| line.starts_with(*prefix))
    {
        Some(Preset::Sam)
    } else {
        None
    }
}

fn extension_preset(path: &Path) -> Option<Preset> {
    let mut name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    for suffix in [".bgzf", ".bgz", ".gz"] {
        if name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
            break;
        }
    }
    if name.ends_with(".bed") {
        Some(Preset::Bed)
    } else if name.ends_with(".gff") || name.ends_with(".gff3") || name.ends_with(".gtf") {
        Some(Preset::Gff)
    } else if name.ends_with(".sam") {
        Some(Preset::Sam)
    } else if name.ends_with(".vcf") {
        Some(Preset::Vcf)
    } else {
        None
    }
}

fn merge_hint(target: &mut Option<Preset>, value: Preset, source: &str) -> Result<()> {
    if target.is_some_and(|current| current != value) {
        Err(invalid(format!("conflicting format {source}")))
    } else {
        *target = Some(value);
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}
