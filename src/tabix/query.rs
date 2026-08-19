mod blocks;
mod selection;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, Write};
use std::num::NonZero;
use std::path::{Path, PathBuf};

use noodles_bgzf::io::Reader;
use rsomics_common::{Context, Result, RsomicsError};
use serde::Serialize;

use crate::bgzip::reader::{TailReader, normalize_truncation};

use self::blocks::BlockReader;
use self::selection::{Selection, TargetSet, read_selections};
use super::{Config, LoadedIndex, load_index};

#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub regions: Vec<String>,
    pub regions_file: Option<PathBuf>,
    pub targets_file: Option<PathBuf>,
    pub index: Option<PathBuf>,
    pub print_header: bool,
    pub header_only: bool,
    pub unique: bool,
    pub separate_regions: bool,
    pub workers: NonZero<usize>,
    pub cache_bytes: usize,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            regions: Vec::new(),
            regions_file: None,
            targets_file: None,
            index: None,
            print_header: false,
            header_only: false,
            unique: false,
            separate_regions: false,
            workers: NonZero::<usize>::MIN,
            cache_bytes: 10 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QuerySummary {
    pub records: u64,
    pub header_lines: u64,
    pub selections: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ListSummary {
    pub references: u64,
}

pub fn query<W>(input: &Path, output: &mut W, options: &QueryOptions) -> Result<QuerySummary>
where
    W: Write + ?Sized,
{
    if options.unique && options.separate_regions {
        return Err(invalid(
            "unique and separate-regions cannot be used together",
        ));
    }
    let index = load_index(input, options.index.as_deref())?;
    let config = Config::from_header(index.header())?;
    let reference_ids = reference_ids(&index);
    let mut selections = match options.regions_file.as_deref() {
        Some(path) => read_selections(path)?,
        None => Vec::new(),
    };
    for region in &options.regions {
        selections.push(Selection::parse(region)?);
    }
    let targets = options
        .targets_file
        .as_deref()
        .map(read_selections)
        .transpose()?
        .map(TargetSet::new);
    validate_references(&selections, targets.as_ref(), &reference_ids)?;
    if options.separate_regions && selections.is_empty() {
        return Err(invalid("separate-regions requires at least one region"));
    }
    if !options.header_only && selections.is_empty() && targets.is_none() {
        return Err(invalid("query requires a region or targets file"));
    }

    let mut reader = if options.header_only {
        None
    } else {
        Some(BlockReader::open(
            input,
            options.workers,
            options.cache_bytes,
        )?)
    };
    let header_lines = if options.print_header || options.header_only {
        write_header(input, output, &config)?
    } else {
        0
    };
    let selection_count =
        u64::try_from(selections.len()).map_err(|_| invalid("selection count exceeds u64"))?;
    if options.header_only {
        output.flush()?;
        return Ok(QuerySummary {
            records: 0,
            header_lines,
            selections: selection_count,
        });
    }

    let reader = reader
        .as_mut()
        .ok_or_else(|| invalid("BGZF query reader is missing"))?;
    let records = if selections.is_empty() {
        query_targets(
            reader,
            output,
            &config,
            targets
                .as_ref()
                .ok_or_else(|| invalid("targets are missing"))?,
            &reference_ids,
        )?
    } else {
        IndexedQuery {
            output,
            config: &config,
            index: &index,
            reference_ids: &reference_ids,
            targets: targets.as_ref(),
            options,
        }
        .run(reader, &selections)?
    };
    output.flush()?;
    Ok(QuerySummary {
        records,
        header_lines,
        selections: selection_count,
    })
}

pub fn list<W>(input: &Path, output: &mut W, explicit_index: Option<&Path>) -> Result<ListSummary>
where
    W: Write + ?Sized,
{
    let index = load_index(input, explicit_index)?;
    Config::from_header(index.header())?;
    let mut references = 0u64;
    for name in index.reference_names() {
        output.write_all(name)?;
        output.write_all(b"\n")?;
        references = references
            .checked_add(1)
            .ok_or_else(|| invalid("reference count overflows u64"))?;
    }
    output.flush()?;
    Ok(ListSummary { references })
}

struct IndexedQuery<'a, W: Write + ?Sized> {
    output: &'a mut W,
    config: &'a Config,
    index: &'a LoadedIndex,
    reference_ids: &'a HashMap<Vec<u8>, usize>,
    targets: Option<&'a TargetSet>,
    options: &'a QueryOptions,
}

impl<W: Write + ?Sized> IndexedQuery<'_, W> {
    fn run(&mut self, reader: &mut BlockReader, selections: &[Selection]) -> Result<u64> {
        let mut records = 0u64;
        let mut seen = HashSet::new();
        for selection in selections {
            if self.options.separate_regions {
                self.output.write_all(&[self.config.comment()])?;
                self.output.write_all(selection.label.as_bytes())?;
                self.output.write_all(b"\n")?;
            }
            let reference_id = self.reference_ids[selection.reference.as_slice()];
            let chunks = self
                .index
                .query_interval(reference_id, selection.interval()?)?;
            reader.read_chunks(&chunks, |offset, line| {
                let line = trim_line(line);
                if self.config.is_comment(line) {
                    return Ok(());
                }
                let record = self.config.parse_unlocated(line).rs_with_context(|| {
                    format!(
                        "parsing record at BGZF virtual offset {}",
                        u64::from(offset)
                    )
                })?;
                if !selection.overlaps(&record)
                    || self
                        .targets
                        .is_some_and(|targets| !targets.overlaps(&record))
                {
                    return Ok(());
                }
                if self.options.unique && !seen.insert(u64::from(offset)) {
                    return Ok(());
                }
                self.output.write_all(line)?;
                self.output.write_all(b"\n")?;
                records = records
                    .checked_add(1)
                    .ok_or_else(|| invalid("query record count overflows u64"))?;
                Ok(())
            })?;
        }
        Ok(records)
    }
}

fn query_targets<W>(
    reader: &mut BlockReader,
    output: &mut W,
    config: &Config,
    targets: &TargetSet,
    reference_ids: &HashMap<Vec<u8>, usize>,
) -> Result<u64>
where
    W: Write + ?Sized,
{
    let mut records = 0u64;
    let mut line_no = 0u64;
    reader.read_all(|_, line| {
        line_no = line_no
            .checked_add(1)
            .ok_or_else(|| invalid("BGZF line count overflows u64"))?;
        let line = trim_line(line);
        if config.is_meta(line_no, line) {
            return Ok(());
        }
        let record = config.parse(line, line_no)?;
        if !reference_ids.contains_key(record.reference) {
            return Err(invalid(format!(
                "data reference {} is not in the selected index",
                String::from_utf8_lossy(record.reference)
            )));
        }
        if targets.overlaps(&record) {
            output.write_all(line)?;
            output.write_all(b"\n")?;
            records = records
                .checked_add(1)
                .ok_or_else(|| invalid("query record count overflows u64"))?;
        }
        Ok(())
    })?;
    Ok(records)
}

fn write_header<W>(input: &Path, output: &mut W, config: &Config) -> Result<u64>
where
    W: Write + ?Sized,
{
    let file =
        File::open(input).rs_with_context(|| format!("opening BGZF data {}", input.display()))?;
    let mut reader = Reader::new(TailReader::new(file));
    let mut line = Vec::new();
    let mut line_no = 0u64;
    let mut written = 0u64;
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(normalize_truncation)?
            == 0
        {
            break;
        }
        line_no = line_no
            .checked_add(1)
            .ok_or_else(|| invalid("BGZF line count overflows u64"))?;
        let trimmed = trim_line(&line);
        if !config.is_meta(line_no, trimmed) {
            break;
        }
        output.write_all(trimmed)?;
        output.write_all(b"\n")?;
        written = written
            .checked_add(1)
            .ok_or_else(|| invalid("header line count overflows u64"))?;
    }
    Ok(written)
}

fn validate_references(
    selections: &[Selection],
    targets: Option<&TargetSet>,
    reference_ids: &HashMap<Vec<u8>, usize>,
) -> Result<()> {
    for reference in selections
        .iter()
        .map(|selection| selection.reference.as_slice())
        .chain(targets.into_iter().flat_map(TargetSet::references))
    {
        if !reference_ids.contains_key(reference) {
            return Err(invalid(format!(
                "reference {} is not in index",
                String::from_utf8_lossy(reference)
            )));
        }
    }
    Ok(())
}

fn reference_ids(index: &LoadedIndex) -> HashMap<Vec<u8>, usize> {
    index
        .reference_names()
        .into_iter()
        .enumerate()
        .map(|(id, name)| (name.to_vec(), id))
        .collect()
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

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}
