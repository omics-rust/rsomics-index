use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use noodles::core::region::Interval;
use noodles::csi::binning_index::BinningIndex;
use noodles::csi::binning_index::index::Header;
use noodles::csi::binning_index::index::reference_sequence::bin::Chunk;
use noodles::{csi, tabix};
use rsomics_common::{Context, Result, RsomicsError};

use crate::bgzip::reader::TailReader;

pub struct LoadedIndex {
    inner: Inner,
    header: Header,
}

enum Inner {
    Tbi(tabix::Index),
    Csi(csi::Index),
}

impl LoadedIndex {
    fn read_seek<R>(mut source: R) -> Result<Self>
    where
        R: Read + Seek,
    {
        let mut probe = noodles_bgzf::io::Reader::new(&mut source);
        let mut magic = [0; 4];
        probe
            .read_exact(&mut magic)
            .map_err(normalize_index_truncation)?;
        drop(probe);
        source.seek(SeekFrom::Start(0))?;

        let inner = match &magic {
            b"TBI\x01" => Inner::Tbi(read_tbi(source)?),
            b"CSI\x01" => Inner::Csi(read_csi(source)?),
            _ => return Err(invalid("index has neither TBI nor CSI magic")),
        };
        let header = match &inner {
            Inner::Tbi(index) => index.header(),
            Inner::Csi(index) => index.header(),
        }
        .cloned()
        .ok_or_else(|| invalid("index has no tabix header"))?;
        validate_header(&header)?;
        let reference_count = match &inner {
            Inner::Tbi(index) => index.reference_sequences().len(),
            Inner::Csi(index) => index.reference_sequences().len(),
        };
        if reference_count != header.reference_sequence_names().len() {
            return Err(invalid(
                "index reference count does not match its reference names",
            ));
        }
        Ok(Self { inner, header })
    }

    pub fn reference_names(&self) -> Vec<&[u8]> {
        self.header
            .reference_sequence_names()
            .iter()
            .map(AsRef::as_ref)
            .collect()
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub(super) fn query_interval(
        &self,
        reference_id: usize,
        interval: Interval,
    ) -> Result<Vec<Chunk>> {
        let mut chunks = match &self.inner {
            Inner::Tbi(index) => index.query(reference_id, interval),
            Inner::Csi(index) => index.query(reference_id, interval),
        }
        .map_err(RsomicsError::Io)?;
        merge_chunks(&mut chunks);
        Ok(chunks)
    }
}

fn merge_chunks(chunks: &mut Vec<Chunk>) {
    chunks.sort_unstable_by_key(|chunk| (chunk.start(), chunk.end()));
    let mut merged: Vec<Chunk> = Vec::with_capacity(chunks.len());
    for &chunk in chunks.iter() {
        match merged.last_mut() {
            Some(previous) if chunk.start() <= previous.end() => {
                *previous = Chunk::new(previous.start(), previous.end().max(chunk.end()));
            }
            _ => merged.push(chunk),
        }
    }
    *chunks = merged;
}

fn read_tbi<R>(source: R) -> Result<tabix::Index>
where
    R: Read,
{
    let mut reader = tabix::io::Reader::new(TailReader::new(source));
    let index = reader
        .read_index()
        .map_err(normalize_index_truncation)
        .rs_context("reading TBI payload")?;
    finish_reader(reader.into_inner())?;
    Ok(index)
}

fn read_csi<R>(source: R) -> Result<csi::Index>
where
    R: Read,
{
    let mut reader = csi::io::Reader::new(TailReader::new(source));
    let index = reader
        .read_index()
        .map_err(normalize_index_truncation)
        .rs_context("reading CSI payload")?;
    finish_reader(reader.into_inner())?;
    Ok(index)
}

fn finish_reader<R>(mut reader: noodles_bgzf::io::Reader<TailReader<R>>) -> Result<()>
where
    R: Read,
{
    let mut trailing = [0];
    if reader
        .read(&mut trailing)
        .map_err(normalize_index_truncation)?
        != 0
    {
        return Err(invalid("index has trailing decompressed bytes"));
    }
    let mut tracked = reader.into_inner();
    if tracked
        .read(&mut trailing)
        .map_err(normalize_index_truncation)?
        != 0
    {
        return Err(invalid("index has trailing compressed bytes"));
    }
    if !tracked.has_complete_eof() {
        return Err(RsomicsError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "index is missing its complete BGZF EOF marker",
        )));
    }
    Ok(())
}

pub fn load_index(data: &Path, explicit: Option<&Path>) -> Result<LoadedIndex> {
    let path = match explicit {
        Some(path) => path.to_owned(),
        None => {
            let tbi = sidecar_path(data, "tbi");
            let csi = sidecar_path(data, "csi");
            if tbi.is_file() {
                tbi
            } else if csi.is_file() {
                csi
            } else {
                return Err(RsomicsError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "no index found for {}; checked {} and {}",
                        data.display(),
                        tbi.display(),
                        csi.display()
                    ),
                )));
            }
        }
    };
    validate_freshness(data, &path)?;
    let file =
        File::open(&path).rs_with_context(|| format!("opening tabix index {}", path.display()))?;
    LoadedIndex::read_seek(file).rs_with_context(|| format!("reading index {}", path.display()))
}

fn validate_header(header: &Header) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for name in header.reference_sequence_names() {
        let name: &[u8] = name.as_ref();
        if name.is_empty()
            || name
                .iter()
                .any(|byte| matches!(byte, 0 | b'\t' | b'\n' | b'\r'))
        {
            return Err(invalid("index contains an invalid reference name"));
        }
        if !seen.insert(name) {
            return Err(invalid("index contains duplicate reference names"));
        }
    }
    Ok(())
}

fn validate_freshness(data: &Path, index: &Path) -> Result<()> {
    let data_modified = std::fs::metadata(data)
        .rs_with_context(|| format!("reading data metadata {}", data.display()))?
        .modified()
        .rs_with_context(|| format!("reading data modification time {}", data.display()))?;
    let index_modified = std::fs::metadata(index)
        .rs_with_context(|| format!("reading index metadata {}", index.display()))?
        .modified()
        .rs_with_context(|| format!("reading index modification time {}", index.display()))?;
    if data_modified > index_modified {
        return Err(invalid(format!(
            "index {} is older than data {}",
            index.display(),
            data.display()
        )));
    }
    Ok(())
}

fn sidecar_path(data: &Path, extension: &str) -> PathBuf {
    let mut value = data.as_os_str().to_os_string();
    value.push(".");
    value.push(extension);
    PathBuf::from(value)
}

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}

fn normalize_index_truncation(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "tabix index payload or BGZF EOF marker is truncated",
        )
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodles_bgzf::VirtualPosition;

    #[test]
    fn overlapping_index_chunks_are_merged_in_stream_order() {
        let position = |value| VirtualPosition::new(0, value).unwrap();
        let mut chunks = vec![
            Chunk::new(position(10), position(20)),
            Chunk::new(position(0), position(5)),
            Chunk::new(position(4), position(12)),
            Chunk::new(position(30), position(40)),
        ];

        merge_chunks(&mut chunks);

        assert_eq!(
            chunks,
            vec![
                Chunk::new(position(0), position(20)),
                Chunk::new(position(30), position(40)),
            ]
        );
    }
}
