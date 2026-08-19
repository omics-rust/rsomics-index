use std::io::{self, Read};

pub(super) const EOF_BLOCK: [u8; 28] = [
    0x1f, 0x8b, 0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x06, 0x00, b'B', b'C', 0x02, 0x00,
    0x1b, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

pub(super) fn read_frame<R>(source: &mut R) -> io::Result<Vec<u8>>
where
    R: Read,
{
    let mut fixed = [0; 12];
    source.read_exact(&mut fixed)?;
    if fixed[..4] != [0x1f, 0x8b, 0x08, 0x04] {
        return Err(invalid("invalid BGZF gzip header"));
    }

    let extra_len = usize::from(u16::from_le_bytes([fixed[10], fixed[11]]));
    let header_len = fixed
        .len()
        .checked_add(extra_len)
        .ok_or_else(|| invalid("BGZF header length overflow"))?;
    if header_len > 65_528 {
        return Err(invalid("BGZF header leaves no room for its trailer"));
    }

    let mut frame = Vec::with_capacity(header_len);
    frame.extend_from_slice(&fixed);
    frame.resize(header_len, 0);
    source.read_exact(&mut frame[fixed.len()..])?;
    let frame_len = parse_block_size(&frame)?;
    if frame_len < header_len + 8 {
        return Err(invalid("BGZF block is shorter than its header and trailer"));
    }
    frame.resize(frame_len, 0);
    source.read_exact(&mut frame[header_len..])?;
    Ok(frame)
}

pub(super) fn parse_block_size(header: &[u8]) -> io::Result<usize> {
    let mut position = 12;
    let mut block_size = None;
    while position < header.len() {
        let fields_end = position
            .checked_add(4)
            .ok_or_else(|| invalid("BGZF extra subfield overflow"))?;
        let fields = header
            .get(position..fields_end)
            .ok_or_else(|| invalid("truncated BGZF extra subfield"))?;
        let data_len = usize::from(u16::from_le_bytes([fields[2], fields[3]]));
        let data_end = fields_end
            .checked_add(data_len)
            .ok_or_else(|| invalid("BGZF extra subfield length overflow"))?;
        let data = header
            .get(fields_end..data_end)
            .ok_or_else(|| invalid("truncated BGZF extra subfield data"))?;
        if fields[..2] == *b"BC" {
            if data.len() != 2 || block_size.is_some() {
                return Err(invalid("invalid or duplicate BGZF BC subfield"));
            }
            block_size = Some(usize::from(u16::from_le_bytes([data[0], data[1]])) + 1);
        }
        position = data_end;
    }
    block_size.ok_or_else(|| invalid("BGZF header has no BC subfield"))
}

pub(super) fn frame_uncompressed_size(frame: &[u8]) -> io::Result<u64> {
    let offset = frame
        .len()
        .checked_sub(4)
        .ok_or_else(|| invalid("truncated BGZF trailer"))?;
    let bytes: [u8; 4] = frame[offset..]
        .try_into()
        .map_err(|_| invalid("truncated BGZF trailer"))?;
    let size = u64::from(u32::from_le_bytes(bytes));
    if size > 65_536 {
        return Err(invalid("BGZF block exceeds the uncompressed-size limit"));
    }
    Ok(size)
}

pub(super) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
