use std::collections::HashSet;

use rsomics_common::{Context, Result, RsomicsError};

use super::{Config, CoordinateSystem, Preset, trim_line_end};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<'a> {
    pub reference: &'a [u8],
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Default)]
pub struct SortedState {
    current_reference: Vec<u8>,
    current_start: u64,
    seen: HashSet<Vec<u8>>,
}

impl SortedState {
    pub fn push(&mut self, record: &Record<'_>, line_no: u64) -> Result<()> {
        if record.reference.is_empty()
            || record.reference.contains(&0)
            || record.start == 0
            || record.end < record.start
        {
            return Err(invalid(format!("line {line_no} has an invalid interval")));
        }
        if self.current_reference == record.reference {
            if record.start < self.current_start {
                return Err(invalid(format!(
                    "line {line_no} is not coordinate sorted on {}",
                    String::from_utf8_lossy(record.reference)
                )));
            }
        } else {
            if self.seen.contains(record.reference) {
                return Err(invalid(format!(
                    "reference {} reappears at line {line_no}",
                    String::from_utf8_lossy(record.reference)
                )));
            }
            self.current_reference.clear();
            self.current_reference.extend_from_slice(record.reference);
            self.seen.insert(self.current_reference.clone());
        }
        self.current_start = record.start;
        Ok(())
    }
}

pub(super) fn parse<'a>(config: &Config, line: &'a [u8], line_no: u64) -> Result<Record<'a>> {
    parse_inner(config, trim_line_end(line))
        .rs_with_context(|| format!("parsing tabix record at line {line_no}"))
}

fn parse_inner<'a>(config: &Config, line: &'a [u8]) -> Result<Record<'a>> {
    if line.is_empty() {
        return Err(invalid("record is empty"));
    }
    let fields = select_fields(config, line)?;
    let reference = required(fields.reference, "reference")?;
    if reference.is_empty() {
        return Err(invalid("reference is empty"));
    }
    if reference.contains(&0) {
        return Err(invalid("reference contains a NUL byte"));
    }
    if config.preset == Some(Preset::Sam) && reference == b"*" {
        return Err(invalid("unmapped SAM record has no indexable reference"));
    }

    let raw_start = parse_u64(required(fields.begin, "begin coordinate")?)?;
    let (start, mut end) = normalize(config, raw_start, fields.end)?;
    match config.preset {
        Some(Preset::Sam) => {
            let span = cigar_reference_span(required(fields.cigar, "SAM CIGAR")?)?;
            end = start
                .checked_add(span - 1)
                .ok_or_else(|| invalid("SAM interval end overflows u64"))?;
        }
        Some(Preset::Vcf) => {
            let reference_allele = required(fields.reference_allele, "VCF REF")?;
            if reference_allele.is_empty() || reference_allele == b"." {
                return Err(invalid("VCF REF must be a nonempty allele"));
            }
            let reference_end = start
                .checked_add(reference_allele.len() as u64 - 1)
                .ok_or_else(|| invalid("VCF reference span overflows u64"))?;
            let info_end = vcf_info_end(required(fields.info, "VCF INFO")?, start)?;
            end = info_end.map_or(reference_end, |value| value.max(reference_end));
        }
        _ => {}
    }
    Ok(Record {
        reference,
        start,
        end,
    })
}

fn normalize(config: &Config, raw_start: u64, raw_end: Option<&[u8]>) -> Result<(u64, u64)> {
    match config.coordinates {
        CoordinateSystem::OneBasedInclusive => {
            if raw_start == 0 {
                return Err(invalid("one-based start must be greater than zero"));
            }
            let end = raw_end.map(parse_u64).transpose()?.unwrap_or(raw_start);
            if end < raw_start {
                return Err(invalid("end coordinate is smaller than start"));
            }
            Ok((raw_start, end))
        }
        CoordinateSystem::ZeroBasedHalfOpen => {
            let end = raw_end
                .map(parse_u64)
                .transpose()?
                .unwrap_or_else(|| raw_start.saturating_add(1));
            if end <= raw_start {
                return Err(invalid(
                    "zero-based half-open end must be greater than start",
                ));
            }
            let start = raw_start
                .checked_add(1)
                .ok_or_else(|| invalid("coordinate overflows u64"))?;
            Ok((start, end))
        }
    }
}

fn cigar_reference_span(cigar: &[u8]) -> Result<u64> {
    if cigar == b"*" {
        return Ok(1);
    }
    if cigar.is_empty() {
        return Err(invalid("SAM CIGAR is empty"));
    }
    let mut length = 0u64;
    let mut span = 0u64;
    let mut digits = 0usize;
    for &byte in cigar {
        if byte.is_ascii_digit() {
            length = length
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                .ok_or_else(|| invalid("SAM CIGAR length overflows u64"))?;
            digits += 1;
            continue;
        }
        if digits == 0 || length == 0 {
            return Err(invalid("SAM CIGAR has an invalid operation length"));
        }
        let operation = byte.to_ascii_uppercase();
        if !matches!(
            operation,
            b'M' | b'I' | b'D' | b'N' | b'S' | b'H' | b'P' | b'=' | b'X'
        ) {
            return Err(invalid("SAM CIGAR has an unknown operation"));
        }
        if matches!(operation, b'M' | b'D' | b'N' | b'=' | b'X') {
            span = span
                .checked_add(length)
                .ok_or_else(|| invalid("SAM CIGAR reference span overflows u64"))?;
        }
        length = 0;
        digits = 0;
    }
    if digits != 0 {
        return Err(invalid("SAM CIGAR ends without an operation"));
    }
    Ok(span.max(1))
}

fn vcf_info_end(info: &[u8], start: u64) -> Result<Option<u64>> {
    if info == b"." {
        return Ok(None);
    }
    let mut end = None;
    let mut seen = false;
    for field in info.split(|byte| *byte == b';') {
        let Some(value) = field.strip_prefix(b"END=") else {
            continue;
        };
        if seen {
            return Err(invalid("VCF INFO contains duplicate END fields"));
        }
        seen = true;
        if value == b"." {
            continue;
        }
        let value = parse_u64(value)?;
        if value < start {
            return Err(invalid("VCF INFO/END is smaller than POS"));
        }
        end = Some(value);
    }
    Ok(end)
}

pub(super) fn parse_u64(bytes: &[u8]) -> Result<u64> {
    if bytes.is_empty() {
        return Err(invalid("integer field is empty"));
    }
    let mut value = 0u64;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return Err(invalid(format!(
                "invalid unsigned integer {}",
                String::from_utf8_lossy(bytes)
            )));
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or_else(|| invalid("unsigned integer overflows u64"))?;
    }
    Ok(value)
}

#[derive(Default)]
struct SelectedFields<'a> {
    reference: Option<&'a [u8]>,
    begin: Option<&'a [u8]>,
    end: Option<&'a [u8]>,
    reference_allele: Option<&'a [u8]>,
    cigar: Option<&'a [u8]>,
    info: Option<&'a [u8]>,
}

fn select_fields<'a>(config: &Config, line: &'a [u8]) -> Result<SelectedFields<'a>> {
    let mut selected = SelectedFields::default();
    let mut count = 0usize;
    for (index, field) in line.split(|byte| *byte == b'\t').enumerate() {
        let column = index + 1;
        count = column;
        if column == config.sequence.get() {
            selected.reference = Some(field);
        }
        if column == config.begin.get() {
            selected.begin = Some(field);
        }
        if config.end.is_some_and(|value| value.get() == column) {
            selected.end = Some(field);
        }
        match (config.preset, column) {
            (Some(Preset::Vcf), 4) => selected.reference_allele = Some(field),
            (Some(Preset::Sam), 6) => selected.cigar = Some(field),
            (Some(Preset::Vcf), 8) => selected.info = Some(field),
            _ => {}
        }
        if column == config.max_column() {
            break;
        }
    }
    if count < config.max_column() {
        return Err(invalid(format!(
            "record has {count} columns but {} are required",
            config.max_column()
        )));
    }
    Ok(selected)
}

fn required<'a>(value: Option<&'a [u8]>, name: &str) -> Result<&'a [u8]> {
    value.ok_or_else(|| invalid(format!("record has no {name} field")))
}

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}
