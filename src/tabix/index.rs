use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use noodles::core::Position;
use noodles::csi::binning_index::BinningIndex;
use noodles::csi::binning_index::index::Header;
use noodles::csi::binning_index::index::reference_sequence::bin::Chunk;
use noodles::{csi, tabix};
use rsomics_common::{Context, Result, RsomicsError};

use crate::bgzip::reader::TailReader;

use super::IndexKind;

pub struct LoadedIndex {
    inner: Inner,
    header: Header,
}

enum Inner {
    Tbi(tabix::Index),
    Csi(csi::Index),
}

impl LoadedIndex {
    pub fn read<R>(mut source: R) -> Result<Self>
    where
        R: Read,
    {
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes)?;
        Self::read_seek(Cursor::new(bytes))
    }

    pub fn read_seek<R>(mut source: R) -> Result<Self>
    where
        R: Read + Seek,
    {
        let mut probe = noodles_bgzf::io::Reader::new(&mut source);
        let mut magic = [0; 4];
        probe.read_exact(&mut magic)?;
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
        Ok(Self { inner, header })
    }

    pub fn kind(&self) -> IndexKind {
        match &self.inner {
            Inner::Tbi(_) => IndexKind::Tbi,
            Inner::Csi(index) => IndexKind::Csi {
                min_shift: index.min_shift(),
            },
        }
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

    pub fn query(&self, reference_id: usize, interval: RangeInclusive<u64>) -> Result<Vec<Chunk>> {
        let start = position(*interval.start())?;
        let end = position(*interval.end())?;
        if end < start {
            return Err(invalid("query end is smaller than start"));
        }
        match &self.inner {
            Inner::Tbi(index) => index.query(reference_id, (start..=end).into()),
            Inner::Csi(index) => index.query(reference_id, (start..=end).into()),
        }
        .map_err(RsomicsError::Io)
    }
}

fn read_tbi<R>(source: R) -> Result<tabix::Index>
where
    R: Read,
{
    let mut reader = tabix::io::Reader::new(TailReader::new(source));
    let index = reader.read_index().rs_context("reading TBI payload")?;
    finish_reader(reader.into_inner())?;
    Ok(index)
}

fn read_csi<R>(source: R) -> Result<csi::Index>
where
    R: Read,
{
    let mut reader = csi::io::Reader::new(TailReader::new(source));
    let index = reader.read_index().rs_context("reading CSI payload")?;
    finish_reader(reader.into_inner())?;
    Ok(index)
}

fn finish_reader<R>(mut reader: noodles_bgzf::io::Reader<TailReader<R>>) -> Result<()>
where
    R: Read,
{
    let mut trailing = [0];
    if reader.read(&mut trailing)? != 0 {
        return Err(invalid("index has trailing decompressed bytes"));
    }
    let mut tracked = reader.into_inner();
    if tracked.read(&mut trailing)? != 0 {
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
    let file =
        File::open(&path).rs_with_context(|| format!("opening tabix index {}", path.display()))?;
    LoadedIndex::read_seek(file).rs_with_context(|| format!("reading index {}", path.display()))
}

fn sidecar_path(data: &Path, extension: &str) -> PathBuf {
    let mut value = data.as_os_str().to_os_string();
    value.push(".");
    value.push(extension);
    PathBuf::from(value)
}

fn position(value: u64) -> Result<Position> {
    let value = usize::try_from(value).map_err(|_| invalid("coordinate exceeds usize"))?;
    Position::try_from(value).map_err(|error| invalid(error.to_string()))
}

fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}
