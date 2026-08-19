use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use noodles::core::Position;
use noodles::core::region::Interval;
use rsomics_common::{Context, Result, RsomicsError};

use crate::tabix::Record;
use crate::tabix::record::parse_u64;

pub(super) struct Selection {
    pub label: String,
    pub reference: Vec<u8>,
    pub start: u64,
    pub end: Option<u64>,
}

impl Selection {
    pub fn parse(value: &str) -> Result<Self> {
        let (reference, start, end) = match value.rsplit_once(':') {
            Some((reference, interval)) => {
                if reference.is_empty() || interval.is_empty() {
                    return Err(invalid(format!("invalid region {value:?}")));
                }
                let (start, end) = match interval.split_once('-') {
                    Some((start, end)) => {
                        let start = if start.is_empty() {
                            1
                        } else {
                            parse_region_coordinate(start)?
                        };
                        let end = if end.is_empty() {
                            None
                        } else {
                            Some(parse_region_coordinate(end)?)
                        };
                        (start, end)
                    }
                    None => {
                        let position = parse_region_coordinate(interval)?;
                        (position, Some(position))
                    }
                };
                (reference.as_bytes(), start, end)
            }
            None if !value.is_empty() => (value.as_bytes(), 1, None),
            None => return Err(invalid("region is empty")),
        };
        if end.is_some_and(|end| end < start) {
            return Err(invalid(format!("region {value:?} has end before start")));
        }
        if reference
            .iter()
            .any(|byte| matches!(byte, 0 | b'\t' | b'\n' | b'\r'))
        {
            return Err(invalid(format!(
                "region {value:?} has an invalid reference"
            )));
        }
        Ok(Self {
            label: value.to_owned(),
            reference: reference.to_vec(),
            start,
            end,
        })
    }

    pub fn interval(&self) -> Result<Interval> {
        let start = position(self.start)?;
        match self.end {
            Some(end) => Ok((start..=position(end)?).into()),
            None => Ok((start..).into()),
        }
    }

    pub fn overlaps(&self, record: &Record<'_>) -> bool {
        record.reference == self.reference
            && record.end >= self.start
            && self.end.is_none_or(|end| record.start <= end)
    }
}

pub(super) fn read_selections(path: &Path) -> Result<Vec<Selection>> {
    let file =
        File::open(path).rs_with_context(|| format!("opening region file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let bed = is_bed(path);
    let mut selections = Vec::new();
    let mut line = Vec::new();
    let mut line_no = 0u64;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        line_no = line_no
            .checked_add(1)
            .ok_or_else(|| invalid("region file line count overflows u64"))?;
        let line = trim_line(&line);
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        selections.push(parse_tabular(line, bed).rs_with_context(|| {
            format!("parsing region file {} at line {line_no}", path.display())
        })?);
    }
    Ok(selections)
}

pub(super) struct TargetSet {
    intervals: HashMap<Vec<u8>, Vec<(u64, u64)>>,
    reference_order: Vec<Vec<u8>>,
}

impl TargetSet {
    pub fn new(selections: Vec<Selection>) -> Self {
        let mut intervals = HashMap::<Vec<u8>, Vec<(u64, u64)>>::new();
        let mut reference_order = Vec::new();
        for selection in selections {
            if !intervals.contains_key(&selection.reference) {
                reference_order.push(selection.reference.clone());
            }
            intervals
                .entry(selection.reference)
                .or_default()
                .push((selection.start, selection.end.unwrap_or(u64::MAX)));
        }
        for ranges in intervals.values_mut() {
            ranges.sort_unstable();
            let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
            for &(start, end) in ranges.iter() {
                match merged.last_mut() {
                    Some((_, current_end)) if start <= current_end.saturating_add(1) => {
                        *current_end = (*current_end).max(end);
                    }
                    _ => merged.push((start, end)),
                }
            }
            *ranges = merged;
        }
        Self {
            intervals,
            reference_order,
        }
    }

    pub fn overlaps(&self, record: &Record<'_>) -> bool {
        self.intervals.get(record.reference).is_some_and(|ranges| {
            let index = ranges.partition_point(|(_, end)| *end < record.start);
            ranges
                .get(index)
                .is_some_and(|(start, _)| *start <= record.end)
        })
    }

    pub fn references(&self) -> impl Iterator<Item = &[u8]> {
        self.reference_order.iter().map(Vec::as_slice)
    }
}

fn parse_tabular(line: &[u8], bed: bool) -> Result<Selection> {
    let mut fields = line.split(|byte| *byte == b'\t');
    let reference = fields
        .next()
        .ok_or_else(|| invalid("region reference is missing"))?;
    let raw_start = fields
        .next()
        .ok_or_else(|| invalid("region start is missing"))?;
    let raw_end = fields
        .next()
        .ok_or_else(|| invalid("region end is missing"))?;
    if reference.is_empty() || reference.contains(&0) {
        return Err(invalid("region reference is invalid"));
    }
    let raw_start = parse_u64(raw_start)?;
    let end = parse_u64(raw_end)?;
    let start = if bed {
        if end <= raw_start {
            return Err(invalid("BED region end must be greater than start"));
        }
        raw_start
            .checked_add(1)
            .ok_or_else(|| invalid("BED region start overflows u64"))?
    } else {
        if raw_start == 0 || end < raw_start {
            return Err(invalid("one-based region coordinates are invalid"));
        }
        raw_start
    };
    Ok(Selection {
        label: format!("{}:{start}-{end}", String::from_utf8_lossy(reference)),
        reference: reference.to_vec(),
        start,
        end: Some(end),
    })
}

fn is_bed(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".bed"))
}

fn trim_line(mut line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\n') {
        line = &line[..line.len() - 1];
    }
    if line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    line
}

fn position(value: u64) -> Result<Position> {
    let value = usize::try_from(value).map_err(|_| invalid("region coordinate exceeds usize"))?;
    Position::try_from(value).map_err(|error| invalid(error.to_string()))
}

fn parse_region_coordinate(value: &str) -> Result<u64> {
    if value.contains(',') {
        let mut groups = value.split(',');
        let first = groups.next().unwrap_or_default();
        if first.is_empty()
            || first.len() > 3
            || !first.bytes().all(|byte| byte.is_ascii_digit())
            || groups
                .any(|group| group.len() != 3 || !group.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(invalid(format!("invalid region coordinate {value:?}")));
        }
    }
    let digits = value
        .as_bytes()
        .iter()
        .copied()
        .filter(|byte| *byte != b',')
        .collect::<Vec<_>>();
    let coordinate =
        parse_u64(&digits).rs_with_context(|| format!("parsing region coordinate {value:?}"))?;
    if coordinate == 0 {
        Err(invalid("region coordinates must be greater than zero"))
    } else {
        Ok(coordinate)
    }
}

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}
