use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use libdeflater::Decompressor;
use noodles_bgzf::VirtualPosition;

use crate::bgzip::{
    GziIndex,
    frame::{decode_member, read_frame},
};

pub(super) enum Source {
    Plain(File),
    Bgzf(Box<BgzfSource>),
}

impl Source {
    pub(super) fn open(
        path: &Path,
        index: Option<Arc<GziIndex>>,
        collect: bool,
    ) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let mut magic = [0; 2];
        let read = file.read(&mut magic)?;
        file.seek(SeekFrom::Start(0))?;
        if read == 2 && magic == [0x1f, 0x8b] {
            Ok(Self::Bgzf(Box::new(BgzfSource {
                reader: BufReader::new(file),
                decoder: Decompressor::new(),
                index,
                data: Vec::new(),
                cursor: 0,
                compressed: 0,
                logical: 0,
                last_empty: false,
                ended: false,
                collect,
                saw_data: false,
                entries: Vec::new(),
            })))
        } else {
            Ok(Self::Plain(file))
        }
    }

    pub(super) fn compressed(&self) -> bool {
        matches!(self, Self::Bgzf(_))
    }

    pub(super) fn scanned_index(self) -> Option<GziIndex> {
        match self {
            Self::Plain(_) => None,
            Self::Bgzf(source) => Some(GziIndex::from_scanned_entries(source.entries)),
        }
    }
}

impl Read for Source {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(source) => source.read(output),
            Self::Bgzf(source) => source.read(output),
        }
    }
}

impl Seek for Source {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Plain(source) => source.seek(position),
            Self::Bgzf(source) => {
                let SeekFrom::Start(offset) = position else {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "BGZF reference seeks require a logical absolute offset",
                    ));
                };
                let index = source
                    .index
                    .as_ref()
                    .ok_or_else(|| invalid("BGZF random access requires a GZI index"))?;
                let position = index.query(offset)?;
                source.reader.seek(SeekFrom::Start(position.compressed()))?;
                source.compressed = position.compressed();
                source.logical = offset - u64::from(position.uncompressed());
                source.data.clear();
                source.cursor = 0;
                source.ended = false;
                source.last_empty = false;
                if !source.next_data()? || usize::from(position.uncompressed()) > source.data.len()
                {
                    return Err(invalid("indexed offset exceeds its BGZF data member"));
                }
                source.cursor = usize::from(position.uncompressed());
                Ok(offset)
            }
        }
    }
}

pub(super) struct BgzfSource {
    reader: BufReader<File>,
    decoder: Decompressor,
    index: Option<Arc<GziIndex>>,
    data: Vec<u8>,
    cursor: usize,
    compressed: u64,
    logical: u64,
    last_empty: bool,
    ended: bool,
    collect: bool,
    saw_data: bool,
    entries: Vec<(u64, u64)>,
}

impl BgzfSource {
    fn next_data(&mut self) -> io::Result<bool> {
        loop {
            if self.reader.fill_buf()?.is_empty() {
                if !self.last_empty {
                    return Err(invalid("BGZF reference has no final empty EOF member"));
                }
                self.ended = true;
                return Ok(false);
            }
            let frame = read_frame(&mut self.reader)?;
            self.data = decode_member(&mut self.decoder, &frame)?;
            self.cursor = 0;
            self.last_empty = self.data.is_empty();
            if !self.last_empty && self.collect {
                if self.saw_data {
                    if VirtualPosition::new(self.compressed, 0).is_none() {
                        return Err(invalid(
                            "BGZF compressed offset exceeds its virtual-position limit",
                        ));
                    }
                    self.entries.push((self.compressed, self.logical));
                }
                self.saw_data = true;
            }
            self.compressed = self
                .compressed
                .checked_add(frame.len() as u64)
                .ok_or_else(|| invalid("BGZF compressed offset overflow"))?;
            self.logical = self
                .logical
                .checked_add(self.data.len() as u64)
                .ok_or_else(|| invalid("BGZF logical offset overflow"))?;
            if !self.last_empty {
                return Ok(true);
            }
        }
    }
}

impl Read for BgzfSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.ended {
            return Ok(0);
        }
        if self.cursor == self.data.len() && !self.next_data()? {
            return Ok(0);
        }
        let count = output.len().min(self.data.len() - self.cursor);
        output[..count].copy_from_slice(&self.data[self.cursor..self.cursor + count]);
        self.cursor += count;
        Ok(count)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
