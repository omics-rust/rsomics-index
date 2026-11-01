use std::collections::HashSet;
use std::io::BufRead;

use rsomics_common::{Result, RsomicsError};

pub(crate) trait FastaVisitor {
    fn start(&mut self, name: String, offset: u64) -> Result<()>;
    fn sequence(&mut self, bytes: &[u8]) -> Result<()>;
    fn line(&mut self, bytes: u64, terminated: bool) -> Result<()>;
    fn finish(&mut self) -> Result<()>;
}

pub(crate) fn invalid(message: impl Into<String>) -> RsomicsError {
    RsomicsError::InvalidInput(message.into())
}

pub(crate) fn small_line<R: BufRead>(reader: &mut R) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            break;
        }
        let newline = memchr::memchr(b'\n', bytes);
        let count = newline.map_or(bytes.len(), |index| index + 1);
        if line.len() + count > 1024 * 1024 {
            return Err(invalid("reference header or index line exceeds 1 MiB"));
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if newline.is_some() {
            break;
        }
    }
    Ok(line)
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(crate) fn scan<R: BufRead, V: FastaVisitor>(reader: &mut R, visitor: &mut V) -> Result<()> {
    let mut offset = 0u64;
    let mut active = false;
    let mut names = HashSet::new();
    loop {
        let Some(first) = reader.fill_buf()?.first().copied() else {
            break;
        };
        if first == b'>' {
            if active {
                visitor.finish()?;
            }
            let header = small_line(reader)?;
            offset = offset
                .checked_add(header.len() as u64)
                .ok_or_else(|| invalid("FASTA byte offset overflow"))?;
            if !header.ends_with(b"\n") {
                return Err(invalid("FASTA header has no sequence"));
            }
            let name = header[1..]
                .split(u8::is_ascii_whitespace)
                .find(|part| !part.is_empty())
                .ok_or_else(|| invalid("empty FASTA name"))?;
            let name = std::str::from_utf8(name)?.to_owned();
            if !valid_name(&name) {
                return Err(invalid("FASTA names must be nonempty printable ASCII"));
            }
            if !names.insert(name.clone()) {
                return Err(invalid(format!("duplicate FASTA name: {name}")));
            }
            visitor.start(name, offset)?;
            active = true;
        } else {
            let (width, terminated) = sequence_line(reader, |bytes| {
                if active {
                    visitor.sequence(bytes)
                } else if bytes.is_empty() {
                    Ok(())
                } else {
                    Err(invalid("sequence data before a FASTA header"))
                }
            })?;
            offset = offset
                .checked_add(width)
                .ok_or_else(|| invalid("FASTA byte offset overflow"))?;
            if active {
                visitor.line(width, terminated)?;
            }
        }
    }
    if !active {
        return Err(invalid("no FASTA records"));
    }
    visitor.finish()
}

fn sequence_line<R: BufRead>(
    reader: &mut R,
    mut emit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<(u64, bool)> {
    let mut pending = None;
    let mut width = 0u64;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            if let Some(byte) = pending {
                emit(&[byte])?;
            }
            return Ok((width, false));
        }
        let newline = memchr::memchr(b'\n', bytes);
        let count = newline.unwrap_or(bytes.len());
        let content = &bytes[..count];
        if newline.is_some() {
            if content.is_empty() {
                if let Some(byte) = pending
                    && byte != b'\r'
                {
                    emit(&[byte])?;
                }
            } else {
                if let Some(byte) = pending {
                    emit(&[byte])?;
                }
                let end = content.len() - usize::from(content.last() == Some(&b'\r'));
                emit(&content[..end])?;
            }
            width = width
                .checked_add((count + 1) as u64)
                .ok_or_else(|| invalid("FASTA line width overflow"))?;
            reader.consume(count + 1);
            return Ok((width, true));
        }
        if let Some(byte) = pending {
            emit(&[byte])?;
        }
        emit(&content[..content.len() - 1])?;
        pending = content.last().copied();
        width = width
            .checked_add(count as u64)
            .ok_or_else(|| invalid("FASTA line width overflow"))?;
        reader.consume(count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[derive(Default)]
    struct Observed {
        headers: Vec<(String, u64)>,
        lines: Vec<(u64, bool)>,
        bases: usize,
        largest_chunk: usize,
        finished: usize,
    }

    impl FastaVisitor for Observed {
        fn start(&mut self, name: String, offset: u64) -> Result<()> {
            self.headers.push((name, offset));
            Ok(())
        }
        fn sequence(&mut self, bytes: &[u8]) -> Result<()> {
            self.bases += bytes.len();
            self.largest_chunk = self.largest_chunk.max(bytes.len());
            Ok(())
        }
        fn line(&mut self, bytes: u64, terminated: bool) -> Result<()> {
            self.lines.push((bytes, terminated));
            Ok(())
        }
        fn finish(&mut self) -> Result<()> {
            self.finished += 1;
            Ok(())
        }
    }

    #[test]
    fn unwrapped_sequences_remain_chunked_across_crlf_and_final_eof() {
        let mut input = b"> a description\r\n".to_vec();
        input.extend(std::iter::repeat_n(b'A', 600_000));
        input.extend_from_slice(b"\r\n>b\nT");
        let mut observed = Observed::default();
        scan(
            &mut BufReader::with_capacity(7, Cursor::new(input)),
            &mut observed,
        )
        .unwrap();
        assert_eq!(
            observed.headers,
            [("a".to_owned(), 17), ("b".to_owned(), 600_022)]
        );
        assert_eq!(observed.lines, [(600_002, true), (1, false)]);
        assert_eq!(observed.bases, 600_001);
        assert!(observed.largest_chunk <= 7);
        assert_eq!(observed.finished, 2);
    }
}
