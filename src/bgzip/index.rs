use std::io::{self, Read, Seek, SeekFrom, Write};

use noodles_bgzf::VirtualPosition;

use super::frame::{EOF_BLOCK, frame_uncompressed_size, invalid, read_frame};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// Checked GZI block offsets for indexed BGZF decompression.
pub struct GziIndex {
    entries: Vec<(u64, u64)>,
}

impl GziIndex {
    /// Scans a complete BGZF stream and builds its GZI entries.
    pub fn scan<R>(source: &mut R) -> io::Result<Self>
    where
        R: Read + Seek,
    {
        source.seek(SeekFrom::Start(0))?;
        let mut entries = Vec::new();
        let mut compressed_offset = 0u64;
        let mut uncompressed_offset = 0u64;
        let mut first = true;

        loop {
            let frame = read_frame(source)?;
            if frame == EOF_BLOCK {
                let mut trailing = [0; 1];
                if source.read(&mut trailing)? != 0 {
                    return Err(invalid("bytes found after the BGZF EOF marker"));
                }
                break;
            }

            let uncompressed_size = frame_uncompressed_size(&frame)?;
            if uncompressed_size == 0 {
                return Err(invalid("noncanonical empty BGZF block"));
            }
            if first {
                first = false;
            } else {
                if VirtualPosition::new(compressed_offset, 0).is_none() {
                    return Err(invalid(
                        "compressed offset exceeds the BGZF virtual-position limit",
                    ));
                }
                entries.push((compressed_offset, uncompressed_offset));
            }
            compressed_offset = compressed_offset
                .checked_add(frame.len() as u64)
                .ok_or_else(|| invalid("compressed offset overflow"))?;
            uncompressed_offset = uncompressed_offset
                .checked_add(uncompressed_size)
                .ok_or_else(|| invalid("uncompressed offset overflow"))?;
        }

        Ok(Self { entries })
    }

    /// Reads and validates a complete GZI stream.
    pub fn read<R>(mut source: R) -> io::Result<Self>
    where
        R: Read,
    {
        let count = read_u64(&mut source)?;
        let count = usize::try_from(count).map_err(|_| invalid("GZI entry count is too large"))?;
        let mut entries = Vec::new();

        let mut previous = (0, 0);
        for _ in 0..count {
            if entries.len() == entries.capacity() {
                let remaining = count - entries.len();
                entries
                    .try_reserve_exact(remaining.min(16_384))
                    .map_err(|_| invalid("GZI entry count is too large"))?;
            }
            let entry = (read_u64(&mut source)?, read_u64(&mut source)?);
            if VirtualPosition::new(entry.0, 0).is_none() {
                return Err(invalid(
                    "GZI compressed offset exceeds the BGZF virtual-position limit",
                ));
            }
            if entry.0 <= previous.0 || entry.1 <= previous.1 {
                return Err(invalid("GZI offsets must be strictly increasing"));
            }
            if entry.0 - previous.0 > 65_536 || entry.1 - previous.1 > 65_536 {
                return Err(invalid("GZI entries do not describe adjacent BGZF blocks"));
            }
            entries.push(entry);
            previous = entry;
        }

        let mut trailing = [0; 1];
        if source.read(&mut trailing)? != 0 {
            return Err(invalid("bytes found after the GZI entries"));
        }

        Ok(Self { entries })
    }

    /// Writes this index in GZI format and flushes the sink.
    pub fn write<W>(&self, mut sink: W) -> io::Result<()>
    where
        W: Write,
    {
        sink.write_all(&(self.entries.len() as u64).to_le_bytes())?;
        for &(compressed_offset, uncompressed_offset) in &self.entries {
            sink.write_all(&compressed_offset.to_le_bytes())?;
            sink.write_all(&uncompressed_offset.to_le_bytes())?;
        }
        sink.flush()
    }

    /// Resolves a zero-based uncompressed byte offset to a BGZF virtual position.
    pub fn query(&self, offset: u64) -> io::Result<VirtualPosition> {
        let position = self
            .entries
            .partition_point(|&(_, uncompressed_offset)| uncompressed_offset <= offset);
        let (compressed_offset, uncompressed_offset) = if position == 0 {
            (0, 0)
        } else {
            self.entries[position - 1]
        };
        let block_offset = u16::try_from(offset - uncompressed_offset)
            .map_err(|_| invalid("uncompressed offset is outside the indexed block"))?;
        VirtualPosition::new(compressed_offset, block_offset)
            .ok_or_else(|| invalid("compressed offset exceeds the BGZF virtual-position limit"))
    }

    /// Returns `(compressed, uncompressed)` offsets for non-origin blocks.
    pub fn entries(&self) -> &[(u64, u64)] {
        &self.entries
    }
}

fn read_u64<R>(source: &mut R) -> io::Result<u64>
where
    R: Read,
{
    let mut bytes = [0; 8];
    source.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
