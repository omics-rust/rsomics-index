use std::num::NonZero;
use std::path::Path;

use rsomics_common::{Context, Result, RsomicsError};

use noodles::csi::binning_index::index::Header;
use noodles::csi::binning_index::index::header::format::{
    CoordinateSystem as HeaderCoordinateSystem, Format,
};

use super::{Record, record, trim_line_end};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Recognized tabix record format.
pub enum Preset {
    /// BED records.
    Bed,
    /// GFF or GTF records.
    Gff,
    /// SAM records.
    Sam,
    /// VCF records.
    Vcf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Coordinate convention used by custom tabular records.
pub enum CoordinateSystem {
    /// One-based coordinates with an inclusive end.
    OneBasedInclusive,
    /// Zero-based coordinates with an exclusive end.
    ZeroBasedHalfOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Checked column and coordinate configuration for tabix records.
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
    /// Returns the canonical configuration for a recognized preset.
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

    /// Creates a checked custom tabular-record configuration.
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

    /// Detects a preset from a path, headers, and a bounded data sample.
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

    pub(crate) fn parse<'a>(&self, line: &'a [u8], line_no: u64) -> Result<Record<'a>> {
        record::parse(self, line, line_no)
    }

    pub(super) fn from_header(header: &Header) -> Result<Self> {
        let (preset, coordinates) = match header.format() {
            Format::Sam => (Some(Preset::Sam), CoordinateSystem::OneBasedInclusive),
            Format::Vcf => (Some(Preset::Vcf), CoordinateSystem::OneBasedInclusive),
            Format::Generic(HeaderCoordinateSystem::Gff) => {
                (None, CoordinateSystem::OneBasedInclusive)
            }
            Format::Generic(HeaderCoordinateSystem::Bed) => {
                (None, CoordinateSystem::ZeroBasedHalfOpen)
            }
        };
        let config = Self {
            preset,
            sequence: header_column(header.reference_sequence_name_index(), "sequence")?,
            begin: header_column(header.start_position_index(), "begin")?,
            end: header
                .end_position_index()
                .map(|index| header_column(index, "end"))
                .transpose()?,
            coordinates,
            comment: header.line_comment_prefix(),
            skip: u64::from(header.line_skip_count()),
        };
        if matches!(config.comment, 0 | b'\t' | b'\n' | b'\r') {
            return Err(invalid("index header has an invalid comment marker"));
        }
        if let Some(preset) = preset {
            let expected = Self::from_preset(preset);
            if config.sequence != expected.sequence
                || config.begin != expected.begin
                || config.end != expected.end
                || config.comment != expected.comment
            {
                return Err(invalid(format!(
                    "index header columns are incompatible with the {preset:?} format"
                )));
            }
        }
        Ok(config)
    }

    pub(super) fn parse_unlocated<'a>(&self, line: &'a [u8]) -> Result<Record<'a>> {
        record::parse_unlocated(self, trim_line_end(line))
    }

    pub(super) fn is_comment(&self, line: &[u8]) -> bool {
        trim_line_end(line).first() == Some(&self.comment)
    }

    pub(crate) fn is_meta(&self, line_no: u64, line: &[u8]) -> bool {
        line_no <= self.skip || trim_line_end(line).first() == Some(&self.comment)
    }

    /// Returns the recognized preset, if this is not a custom configuration.
    pub fn preset(&self) -> Option<Preset> {
        self.preset
    }

    /// Returns the one-based reference-name column.
    pub fn sequence_column(&self) -> usize {
        self.sequence.get()
    }

    /// Returns the one-based start-coordinate column.
    pub fn begin_column(&self) -> usize {
        self.begin.get()
    }

    /// Returns the optional one-based end-coordinate column.
    pub fn end_column(&self) -> Option<usize> {
        self.end.map(NonZero::get)
    }

    /// Returns the configured coordinate convention.
    pub fn coordinate_system(&self) -> CoordinateSystem {
        self.coordinates
    }

    /// Returns the header and comment prefix byte.
    pub fn comment(&self) -> u8 {
        self.comment
    }

    /// Returns the number of leading lines skipped before parsing records.
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

fn header_column(index: usize, name: &str) -> Result<NonZero<usize>> {
    index
        .checked_add(1)
        .and_then(NonZero::new)
        .ok_or_else(|| invalid(format!("index header {name} column overflows usize")))
}

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_uses_format_evidence_and_rejects_ambiguity() {
        let fixtures = [
            (
                "records.bed.gz",
                include_bytes!("../../tests/golden/records.bed").as_slice(),
                Preset::Bed,
            ),
            (
                "records.gff3",
                include_bytes!("../../tests/golden/records.gff").as_slice(),
                Preset::Gff,
            ),
            (
                "records.sam",
                include_bytes!("../../tests/golden/records.sam").as_slice(),
                Preset::Sam,
            ),
            (
                "records.vcf.bgz",
                include_bytes!("../../tests/golden/records.vcf").as_slice(),
                Preset::Vcf,
            ),
        ];

        for (name, sample, expected) in fixtures {
            let config = Config::detect(Some(Path::new(name)), sample).unwrap();
            assert_eq!(config.preset(), Some(expected));
        }

        let ambiguous = b"read1\t0\tchr1\t1\t60\t5M\t*\t0\t0\tA\tF";
        let error = Config::detect(None, ambiguous).unwrap_err();
        assert!(error.to_string().contains("--preset"), "{error}");
        assert_eq!(
            Config::detect(Some(Path::new("empty.bed")), b"")
                .unwrap()
                .preset(),
            Some(Preset::Bed)
        );
        assert!(Config::detect(None, b"").is_err());

        let header_only = b"##fileformat=VCFv4.3\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n";
        assert_eq!(
            Config::detect(None, header_only).unwrap().preset(),
            Some(Preset::Vcf)
        );
    }

    #[test]
    fn detection_rejects_conflicting_header_and_extension() {
        let error = Config::detect(
            Some(Path::new("calls.bed")),
            include_bytes!("../../tests/golden/records.vcf"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("conflict"), "{error}");
    }
}
