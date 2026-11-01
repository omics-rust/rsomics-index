use std::collections::HashMap;
use std::io::{BufRead, Write};

use rsomics_common::Result;

use crate::reference::{FastaVisitor, invalid, small_line, valid_name};

#[derive(Clone, Debug)]
pub(super) struct Record {
    pub(super) name: String,
    pub(super) length: u64,
    pub(super) offset: u64,
    pub(super) line_bases: u64,
    pub(super) line_width: u64,
}

impl Record {
    pub(super) fn byte_offset(&self, position: u64) -> Result<u64> {
        (position / self.line_bases)
            .checked_mul(self.line_width)
            .and_then(|value| value.checked_add(position % self.line_bases))
            .and_then(|value| self.offset.checked_add(value))
            .ok_or_else(|| invalid("FAI byte offset overflow"))
    }
}

#[derive(Default)]
pub(super) struct Index {
    pub(super) records: Vec<Record>,
    pub(super) names: HashMap<String, usize>,
}

impl Index {
    pub(super) fn read<R: BufRead>(reader: &mut R) -> Result<Self> {
        let mut index = Self::default();
        loop {
            let line = small_line(reader)?;
            if line.is_empty() {
                break;
            }
            let text = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            let fields: Vec<_> = text.split('\t').collect();
            if fields.len() != 5 || !valid_name(fields[0]) {
                return Err(invalid(
                    "FASTA FAI requires a name and exactly four numeric fields",
                ));
            }
            let numbers = fields[1..]
                .iter()
                .map(|value| value.parse::<u64>())
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let record = Record {
                name: fields[0].to_owned(),
                length: numbers[0],
                offset: numbers[1],
                line_bases: numbers[2],
                line_width: numbers[3],
            };
            if record.length == 0
                || record.line_bases == 0
                || record.line_bases > record.length
                || record.line_width < record.line_bases
                || record.line_width - record.line_bases > 2
            {
                return Err(invalid("invalid FAI length or line layout"));
            }
            record.byte_offset(record.length - 1)?;
            index.push(record)?;
        }
        if index.records.is_empty() {
            return Err(invalid("empty FASTA index"));
        }
        Ok(index)
    }

    pub(super) fn write(&self, output: &mut impl Write) -> Result<()> {
        for record in &self.records {
            writeln!(
                output,
                "{}\t{}\t{}\t{}\t{}",
                record.name, record.length, record.offset, record.line_bases, record.line_width
            )?;
        }
        Ok(())
    }

    fn push(&mut self, record: Record) -> Result<()> {
        if self
            .names
            .insert(record.name.clone(), self.records.len())
            .is_some()
        {
            return Err(invalid("duplicate name in FAI"));
        }
        self.records.push(record);
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Builder {
    pub(super) index: Index,
    current: Option<Record>,
    bases: u64,
    previous: Option<(u64, u64)>,
    separated: bool,
}

impl FastaVisitor for Builder {
    fn start(&mut self, name: String, offset: u64) -> Result<()> {
        self.current = Some(Record {
            name,
            length: 0,
            offset,
            line_bases: 0,
            line_width: 0,
        });
        self.previous = None;
        self.bases = 0;
        self.separated = false;
        Ok(())
    }

    fn sequence(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.iter().any(|byte| !byte.is_ascii_graphic()) {
            return Err(invalid(
                "indexable FASTA sequence contains whitespace or non-ASCII bytes",
            ));
        }
        self.bases = self
            .bases
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("FASTA line length overflow"))?;
        Ok(())
    }

    fn line(&mut self, bytes: u64, terminated: bool) -> Result<()> {
        let record = self
            .current
            .as_mut()
            .ok_or_else(|| invalid("FASTA record is absent"))?;
        if self.bases == 0 {
            self.separated = true;
            return Ok(());
        }
        if self.separated {
            return Err(invalid("blank line within an indexed FASTA sequence"));
        }
        let width = bytes
            .checked_add(u64::from(!terminated))
            .ok_or_else(|| invalid("FASTA line width overflow"))?;
        if let Some(previous) = self.previous {
            if previous != (record.line_bases, record.line_width)
                || self.bases > record.line_bases
                || width > record.line_width
            {
                return Err(invalid(format!(
                    "inconsistent FASTA line layout: {}",
                    record.name
                )));
            }
        } else {
            record.line_bases = self.bases;
            record.line_width = width;
        }
        record.length = record
            .length
            .checked_add(self.bases)
            .ok_or_else(|| invalid("FASTA sequence length overflow"))?;
        self.previous = Some((self.bases, width));
        self.bases = 0;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let record = self
            .current
            .take()
            .ok_or_else(|| invalid("FASTA record is absent"))?;
        if record.length == 0 {
            return Err(invalid(format!("empty FASTA sequence: {}", record.name)));
        }
        self.index.push(record)
    }
}
