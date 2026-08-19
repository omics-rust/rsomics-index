use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, Seek, SeekFrom};
use std::path::Path;

use noodles::csi::binning_index::index::header::format::{
    CoordinateSystem as HeaderCoordinateSystem, Format,
};
use noodles::csi::binning_index::index::header::{
    Builder as HeaderBuilder, ReferenceSequenceNames,
};
use noodles::csi::binning_index::index::reference_sequence::index::BinnedIndex;
use noodles::csi::binning_index::index::reference_sequence::{Bin, Metadata, bin::Chunk};
use noodles::csi::binning_index::index::{Builder as IndexBuilder, ReferenceSequence};
use noodles::{csi, tabix};
use noodles_bgzf::{VirtualPosition, io::Reader};
use rsomics_common::{Context, Result, RsomicsError, reject_output_alias, write_atomic};
use serde::Serialize;

use crate::bgzip::reader::{TailReader, normalize_truncation};

use super::record::parse_u64;
use super::{Config, CoordinateSystem, Preset, SortedState, trim_line_end};

const TBI_MIN_SHIFT: u8 = 14;
const TBI_DEPTH: u8 = 5;
const TBI_LIMIT: u64 = 1 << 29;
const MAX_CSI_DEPTH: u8 = 9;
const HTS_MAX_SHIFT: u8 = 31;
const MIN_MARKER_DISTANCE: u64 = 0x10000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Tabix index encoding to build.
pub enum IndexKind {
    /// A TBI index with the fixed `2^29` coordinate limit.
    Tbi,
    /// A CSI index with a caller-selected minimum interval shift.
    Csi {
        /// Minimum interval shift in the inclusive range 1 through 31.
        min_shift: u8,
    },
}

#[derive(Debug, Clone, Copy)]
/// Format and encoding options for tabix index construction.
pub struct BuildOptions {
    /// Checked record-format configuration.
    pub config: Config,
    /// Index encoding to create.
    pub kind: IndexKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// Counts and encoding for a completed index build.
pub struct BuildSummary {
    /// Index encoding written.
    pub kind: IndexKind,
    /// Data records indexed.
    pub records: u64,
    /// Distinct references indexed.
    pub references: u64,
}

/// Builds an index into an already-open regular file.
pub fn build(input: &Path, output: &mut File, options: &BuildOptions) -> Result<BuildSummary> {
    let accumulated = accumulate(input, options)?;
    let summary = BuildSummary {
        kind: options.kind,
        records: accumulated.records,
        references: accumulated.names.len() as u64,
    };
    output.seek(SeekFrom::Start(0))?;
    output.set_len(0)?;
    match options.kind {
        IndexKind::Tbi => write_tbi(output, build_tbi(accumulated, &options.config)?)?,
        IndexKind::Csi { min_shift } => {
            write_csi(output, build_csi(accumulated, &options.config, min_shift)?)?
        }
    }
    Ok(summary)
}

/// Builds and transactionally commits a named index.
pub fn build_named(input: &Path, output: &Path, options: &BuildOptions) -> Result<BuildSummary> {
    reject_output_alias(output, [input])?;
    write_atomic(output, |file| build(input, file, options))
}

struct Accumulated {
    names: Vec<Vec<u8>>,
    references: Vec<ReferenceAccum>,
    records: u64,
    depth: Option<u8>,
}

fn accumulate(input: &Path, options: &BuildOptions) -> Result<Accumulated> {
    validate_kind(options.kind)?;
    let file =
        File::open(input).rs_with_context(|| format!("opening BGZF input {}", input.display()))?;
    let tracked = TailReader::new(file);
    let mut reader = Reader::new(tracked);
    let mut line = Vec::new();
    let mut line_no = 0u64;
    let mut sorted = SortedState::default();
    let mut names = Vec::<Vec<u8>>::new();
    let mut references = Vec::<ReferenceAccum>::new();
    let mut records = 0u64;
    let mut declared_max = None;
    let mut depth = None;

    loop {
        let chunk_start = reader.virtual_position();
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
            .ok_or_else(|| invalid("line count overflows u64"))?;
        let chunk_end = reader.virtual_position();
        let trimmed = trim_line_end(&line);
        if options.config.is_meta(line_no, trimmed) {
            if let Some(length) = declared_length(options.config.preset(), trimmed)? {
                declared_max = Some(declared_max.map_or(length, |value: u64| value.max(length)));
                if let (IndexKind::Csi { min_shift }, Some(current_depth)) = (options.kind, depth)
                    && u128::from(length) > coordinate_capacity(min_shift, current_depth)?
                {
                    return Err(invalid(format!(
                        "declared reference length at line {line_no} exceeds CSI capacity"
                    )));
                }
            }
            continue;
        }

        let record = options.config.parse(trimmed, line_no)?;
        sorted.push(&record, line_no)?;
        let reference_id = intern_reference(&mut names, &mut references, record.reference);
        let chunk = Chunk::new(chunk_start, chunk_end);
        match options.kind {
            IndexKind::Tbi => {
                if record.start > TBI_LIMIT || record.end > TBI_LIMIT {
                    return Err(invalid(format!(
                        "record at line {line_no} reaches the TBI coordinate limit"
                    )));
                }
                references[reference_id].add(
                    reg2bin(record.start, record.end, TBI_MIN_SHIFT, TBI_DEPTH)?,
                    record.end,
                    TBI_MIN_SHIFT,
                    chunk,
                )?;
            }
            IndexKind::Csi { min_shift } => {
                let current_depth = match depth {
                    Some(depth) => depth,
                    None => {
                        let resolved = csi_depth(min_shift, options.config.preset(), declared_max)?;
                        depth = Some(resolved);
                        resolved
                    }
                };
                let capacity = coordinate_capacity(min_shift, current_depth)?;
                if u128::from(record.end) > capacity {
                    return Err(invalid(format!(
                        "record at line {line_no} exceeds CSI capacity for min shift {min_shift}"
                    )));
                }
                references[reference_id].add(
                    reg2bin(record.start, record.end, min_shift, current_depth)?,
                    record.end,
                    min_shift,
                    chunk,
                )?;
            }
        }
        records = records
            .checked_add(1)
            .ok_or_else(|| invalid("record count overflows u64"))?;
    }

    let tracked = reader.into_inner();
    if !tracked.has_complete_eof() {
        return Err(RsomicsError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "BGZF input is missing its complete EOF marker",
        )));
    }
    if let IndexKind::Csi { min_shift } = options.kind
        && depth.is_none()
    {
        depth = Some(csi_depth(min_shift, options.config.preset(), declared_max)?);
    }
    Ok(Accumulated {
        names,
        references,
        records,
        depth,
    })
}

fn intern_reference(
    names: &mut Vec<Vec<u8>>,
    references: &mut Vec<ReferenceAccum>,
    name: &[u8],
) -> usize {
    if names.last().is_some_and(|current| current == name) {
        names.len() - 1
    } else {
        names.push(name.to_vec());
        references.push(ReferenceAccum::default());
        names.len() - 1
    }
}

#[derive(Default)]
struct ReferenceAccum {
    bins: HashMap<usize, Vec<Chunk>>,
    linear: LinearOffsets,
    first_offset: Option<VirtualPosition>,
    last_offset: VirtualPosition,
    records: u64,
}

impl ReferenceAccum {
    fn add(&mut self, bin: usize, end: u64, min_shift: u8, chunk: Chunk) -> Result<()> {
        let chunks = self.bins.entry(bin).or_default();
        match chunks.last_mut() {
            Some(last) if chunk.start() <= last.end() => {
                *last = Chunk::new(last.start(), chunk.end());
            }
            _ => chunks.push(chunk),
        }
        let window = usize::try_from((end - 1) >> min_shift)
            .map_err(|_| invalid("linear-index window exceeds usize"))?;
        self.linear.insert(window, chunk.start());
        self.first_offset.get_or_insert(chunk.start());
        self.last_offset = self.last_offset.max(chunk.end());
        self.records = self
            .records
            .checked_add(1)
            .ok_or_else(|| invalid("reference record count overflows u64"))?;
        Ok(())
    }

    fn metadata(&self) -> Option<Metadata> {
        self.first_offset
            .map(|first| Metadata::new(first, self.last_offset, self.records, 0))
    }
}

#[derive(Default)]
struct LinearOffsets {
    segments: Vec<(usize, VirtualPosition)>,
}

impl LinearOffsets {
    fn insert(&mut self, end: usize, offset: VirtualPosition) {
        if self
            .segments
            .last()
            .is_none_or(|(current_end, _)| end > *current_end)
        {
            self.segments.push((end, offset));
        }
    }

    fn get(&self, window: usize) -> VirtualPosition {
        let index = self
            .segments
            .partition_point(|(segment_end, _)| *segment_end < window);
        self.segments
            .get(index)
            .map_or(VirtualPosition::MIN, |(_, offset)| *offset)
    }

    fn dense(&self) -> Vec<VirtualPosition> {
        let Some((last, _)) = self.segments.last() else {
            return Vec::new();
        };
        let mut output = Vec::with_capacity(last + 1);
        let mut start = 0;
        for &(end, offset) in &self.segments {
            output.resize(end + 1, offset);
            start = end + 1;
        }
        debug_assert_eq!(start, output.len());
        output
    }
}

fn build_tbi(accumulated: Accumulated, config: &Config) -> Result<tabix::Index> {
    let header = header(config, &accumulated.names)?;
    let reference_sequences: Vec<ReferenceSequence<Vec<VirtualPosition>>> = accumulated
        .references
        .into_iter()
        .map(|reference| {
            let metadata = reference.metadata();
            let bins = compress_bins(reference.bins, TBI_DEPTH)
                .into_iter()
                .map(|(id, chunks)| (id, Bin::new(chunks)))
                .collect();
            ReferenceSequence::new(bins, reference.linear.dense(), metadata)
        })
        .collect();
    Ok(IndexBuilder::<Vec<VirtualPosition>>::default()
        .set_min_shift(TBI_MIN_SHIFT)
        .set_depth(TBI_DEPTH)
        .set_header(header)
        .set_reference_sequences(reference_sequences)
        .set_unplaced_unmapped_record_count(0)
        .build())
}

fn build_csi(accumulated: Accumulated, config: &Config, min_shift: u8) -> Result<csi::Index> {
    let depth = accumulated
        .depth
        .ok_or_else(|| invalid("CSI depth was not resolved"))?;
    let header = header(config, &accumulated.names)?;
    let reference_sequences: Vec<ReferenceSequence<BinnedIndex>> = accumulated
        .references
        .into_iter()
        .map(|reference| {
            let metadata = reference.metadata();
            let compressed = compress_bins(reference.bins, depth);
            let index = compressed
                .iter()
                .map(|(id, _)| (*id, reference.linear.get(bin_bottom(*id, depth))))
                .collect();
            let bins = compressed
                .into_iter()
                .map(|(id, chunks)| (id, Bin::new(chunks)))
                .collect();
            ReferenceSequence::new(bins, index, metadata)
        })
        .collect();
    Ok(IndexBuilder::<BinnedIndex>::default()
        .set_min_shift(min_shift)
        .set_depth(depth)
        .set_header(header)
        .set_reference_sequences(reference_sequences)
        .set_unplaced_unmapped_record_count(0)
        .build())
}

fn header(
    config: &Config,
    names: &[Vec<u8>],
) -> Result<noodles::csi::binning_index::index::Header> {
    let format = match config.preset() {
        Some(Preset::Sam) => Format::Sam,
        Some(Preset::Vcf) => Format::Vcf,
        _ => Format::Generic(match config.coordinate_system() {
            CoordinateSystem::OneBasedInclusive => HeaderCoordinateSystem::Gff,
            CoordinateSystem::ZeroBasedHalfOpen => HeaderCoordinateSystem::Bed,
        }),
    };
    let names: ReferenceSequenceNames = names.iter().map(|name| name.as_slice().into()).collect();
    let skip = u32::try_from(config.skip()).map_err(|_| invalid("line skip exceeds u32"))?;
    Ok(HeaderBuilder::default()
        .set_format(format)
        .set_reference_sequence_name_index(config.sequence_column() - 1)
        .set_start_position_index(config.begin_column() - 1)
        .set_end_position_index(config.end_column().map(|column| column - 1))
        .set_line_comment_prefix(config.comment())
        .set_line_skip_count(skip)
        .set_reference_sequence_names(names)
        .build())
}

fn write_tbi(output: &mut File, index: tabix::Index) -> Result<()> {
    let mut writer = tabix::io::Writer::new(output);
    writer.write_index(&index)?;
    writer.into_inner().finish()?;
    Ok(())
}

fn write_csi(output: &mut File, index: csi::Index) -> Result<()> {
    let mut writer = csi::io::Writer::new(output);
    writer.write_index(&index)?;
    writer.into_inner().finish()?;
    Ok(())
}

fn validate_kind(kind: IndexKind) -> Result<()> {
    if let IndexKind::Csi { min_shift } = kind
        && !(1..=HTS_MAX_SHIFT).contains(&min_shift)
    {
        return Err(invalid("CSI minimum shift must be between 1 and 31"));
    }
    Ok(())
}

fn csi_depth(min_shift: u8, preset: Option<Preset>, declared_max: Option<u64>) -> Result<u8> {
    validate_kind(IndexKind::Csi { min_shift })?;
    let formatted = matches!(preset, Some(Preset::Sam | Preset::Vcf));
    let mut depth = if formatted && declared_max.is_some() {
        (HTS_MAX_SHIFT - min_shift).div_ceil(3).min(MAX_CSI_DEPTH)
    } else if min_shift < 10 {
        MAX_CSI_DEPTH
    } else if min_shift < 25 {
        MAX_CSI_DEPTH - (min_shift - 10) / 3
    } else {
        4
    };
    let target = declared_max
        .map(|length| u128::from(length) + 256)
        .unwrap_or(1);
    while coordinate_capacity(min_shift, depth)? < target {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| invalid("CSI depth overflows u8"))?;
        if depth > MAX_CSI_DEPTH {
            return Err(invalid("coordinates exceed the maximum CSI depth"));
        }
    }
    let shift = u32::from(min_shift) + 3 * u32::from(depth);
    if shift >= usize::BITS {
        return Err(invalid(
            "CSI minimum shift and depth exceed the platform coordinate width",
        ));
    }
    Ok(depth)
}

fn coordinate_capacity(min_shift: u8, depth: u8) -> Result<u128> {
    1u128
        .checked_shl(u32::from(min_shift) + 3 * u32::from(depth))
        .ok_or_else(|| invalid("CSI coordinate capacity overflows u128"))
}

fn declared_length(preset: Option<Preset>, line: &[u8]) -> Result<Option<u64>> {
    let value = match preset {
        Some(Preset::Vcf) if line.starts_with(b"##contig=<") => line
            .split(|byte| *byte == b',')
            .find_map(|field| field.strip_prefix(b"length="))
            .map(|field| field.strip_suffix(b">").unwrap_or(field)),
        Some(Preset::Sam) if line.starts_with(b"@SQ\t") => line
            .split(|byte| *byte == b'\t')
            .find_map(|field| field.strip_prefix(b"LN:")),
        _ => None,
    };
    value.map(parse_u64).transpose()
}

fn reg2bin(start: u64, end: u64, min_shift: u8, depth: u8) -> Result<usize> {
    let begin = start
        .checked_sub(1)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid("record start does not fit the index coordinate width"))?;
    let end = end
        .checked_sub(1)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid("record end does not fit the index coordinate width"))?;
    let mut level = depth;
    let mut shift = min_shift;
    let mut offset = ((1usize << (depth * 3)) - 1) / 7;
    while level > 0 {
        if begin >> shift == end >> shift {
            return Ok(offset + (begin >> shift));
        }
        level -= 1;
        shift += 3;
        offset -= 1usize << (level * 3);
    }
    Ok(0)
}

fn compress_bins(mut bins: HashMap<usize, Vec<Chunk>>, depth: u8) -> Vec<(usize, Vec<Chunk>)> {
    for target_level in (1..=u32::from(depth)).rev() {
        let candidates = bins
            .keys()
            .copied()
            .filter(|bin| bin_level(*bin) == target_level)
            .collect::<Vec<_>>();
        for bin in candidates {
            let chunks = bins.get_mut(&bin).expect("candidate bin exists");
            if target_level < u32::from(depth) {
                chunks.sort_by_key(|chunk| (chunk.start(), chunk.end()));
            }
            if !small_bin(chunks) {
                continue;
            }
            let parent = (bin - 1) >> 3;
            if !bins.contains_key(&parent) {
                continue;
            }
            let chunks = bins.remove(&bin).expect("candidate bin exists");
            bins.get_mut(&parent)
                .expect("checked parent exists")
                .extend(chunks);
        }
    }
    if let Some(root) = bins.get_mut(&0) {
        root.sort_by_key(|chunk| (chunk.start(), chunk.end()));
    }
    for chunks in bins.values_mut() {
        merge_chunks(chunks);
    }
    let mut bins = bins.into_iter().collect::<Vec<_>>();
    bins.sort_by_key(|(id, _)| *id);
    bins
}

fn small_bin(chunks: &[Chunk]) -> bool {
    match (chunks.first(), chunks.last()) {
        (Some(first), Some(last)) => {
            last.end().compressed() - first.start().compressed() < MIN_MARKER_DISTANCE
        }
        _ => true,
    }
}

fn merge_chunks(chunks: &mut Vec<Chunk>) {
    chunks.sort_by_key(|chunk| (chunk.start(), chunk.end()));
    let mut merged: Vec<Chunk> = Vec::with_capacity(chunks.len());
    for &chunk in chunks.iter() {
        match merged.last_mut() {
            Some(last) if last.end().compressed() >= chunk.start().compressed() => {
                if chunk.end() > last.end() {
                    *last = Chunk::new(last.start(), chunk.end());
                }
            }
            _ => merged.push(chunk),
        }
    }
    *chunks = merged;
}

fn bin_level(mut bin: usize) -> u32 {
    let mut level = 0;
    while bin != 0 {
        bin = (bin - 1) >> 3;
        level += 1;
    }
    level
}

fn bin_bottom(bin: usize, depth: u8) -> usize {
    let level = bin_level(bin);
    let first = ((1usize << (3 * level)) - 1) / 7;
    (bin - first) << ((u32::from(depth) - level) * 3)
}

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csi_defaults_follow_the_requested_minimum_shift() {
        assert!(csi_depth(0, None, None).is_err());
        assert_eq!(csi_depth(10, None, None).unwrap(), 9);
        assert_eq!(csi_depth(14, None, None).unwrap(), 8);
        assert_eq!(csi_depth(25, None, None).unwrap(), 4);
        assert_eq!(csi_depth(31, None, None).unwrap(), 4);
    }

    #[test]
    fn declared_lengths_start_from_the_format_specific_floor() {
        assert_eq!(csi_depth(10, Some(Preset::Vcf), Some(1000)).unwrap(), 7);
        assert_eq!(csi_depth(14, Some(Preset::Sam), Some(1000)).unwrap(), 6);
        assert!(csi_depth(1, Some(Preset::Vcf), Some(1 << 40)).is_err());
    }
}
