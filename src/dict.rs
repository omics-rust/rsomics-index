use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::MultiGzDecoder;
use md5::{Digest, Md5};
use rsomics_common::{Result, reject_output_alias, write_output};
use serde::Serialize;

use crate::output::{ensure_replaceable, named_path};
use crate::reference::{FastaVisitor, invalid, scan};

pub(crate) struct Options {
    pub(crate) input: PathBuf,
    pub(crate) output: Option<PathBuf>,
    pub(crate) uri: Option<String>,
    pub(crate) assembly: Option<String>,
    pub(crate) species: Option<String>,
    pub(crate) alias: bool,
    pub(crate) no_header: bool,
    pub(crate) force: bool,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Summary {
    references: u64,
    bases: u64,
}

pub(crate) fn run(options: &Options) -> Result<Summary> {
    if let Some(output) = named_path(options.output.as_deref()) {
        reject_output_alias(output, [options.input.as_path()])?;
        ensure_replaceable(output, options.force)?;
    }
    let uri = match &options.uri {
        Some(uri) => Some(uri.clone()),
        None if options.input == Path::new("-") => None,
        None => Some(format!(
            "file://{}",
            options.input.canonicalize()?.display()
        )),
    };
    for value in uri
        .iter()
        .chain(options.assembly.iter())
        .chain(options.species.iter())
    {
        if value
            .bytes()
            .any(|byte| matches!(byte, b'\t' | b'\n' | b'\r' | 0))
        {
            return Err(invalid(
                "dictionary metadata contains a SAM field delimiter",
            ));
        }
    }
    let input: Box<dyn Read> = if options.input == Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(&options.input)?)
    };
    let mut input = BufReader::new(input);
    let mut prefix = Vec::with_capacity(2);
    input.by_ref().take(2).read_to_end(&mut prefix)?;
    let compressed = prefix.starts_with(&[0x1f, 0x8b]);
    let input = io::Cursor::new(prefix).chain(input);
    let decoded: Box<dyn Read> = if compressed {
        Box::new(MultiGzDecoder::new(input))
    } else {
        Box::new(input)
    };
    write_output(options.output.as_deref(), |output| {
        if !options.no_header {
            writeln!(output, "@HD\tVN:1.0\tSO:unsorted")?;
        }
        let mut visitor = Dictionary {
            output,
            options,
            uri: uri.as_deref(),
            current: None,
            md5: Md5::new(),
            length: 0,
            buffer: Vec::with_capacity(64 * 1024),
            summary: Summary::default(),
        };
        scan(
            &mut BufReader::with_capacity(256 * 1024, decoded),
            &mut visitor,
        )?;
        Ok(visitor.summary)
    })
}

struct Dictionary<'a> {
    output: &'a mut dyn Write,
    options: &'a Options,
    uri: Option<&'a str>,
    current: Option<String>,
    md5: Md5,
    length: u64,
    buffer: Vec<u8>,
    summary: Summary,
}

impl FastaVisitor for Dictionary<'_> {
    fn start(&mut self, name: String, _offset: u64) -> Result<()> {
        if name.starts_with(['*', '='])
            || name.bytes().any(|byte| b"\\,\"`'()[]{}<>".contains(&byte))
        {
            return Err(invalid(
                "FASTA name cannot be represented as a SAM SN field",
            ));
        }
        self.current = Some(name);
        self.length = 0;
        self.md5 = Md5::new();
        self.buffer.clear();
        Ok(())
    }

    fn sequence(&mut self, bytes: &[u8]) -> Result<()> {
        for chunk in bytes.chunks(16 * 1024) {
            let previous = self.buffer.len();
            self.buffer.extend(
                chunk
                    .iter()
                    .copied()
                    .filter(u8::is_ascii_graphic)
                    .map(|byte| byte.to_ascii_uppercase()),
            );
            self.length = self
                .length
                .checked_add((self.buffer.len() - previous) as u64)
                .ok_or_else(|| invalid("dictionary sequence length overflow"))?;
            if self.buffer.len() >= 64 * 1024 {
                self.md5.update(&self.buffer);
                self.buffer.clear();
            }
        }
        Ok(())
    }

    fn line(&mut self, _bytes: u64, _terminated: bool) -> Result<()> {
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let name = self
            .current
            .take()
            .ok_or_else(|| invalid("dictionary sequence is absent"))?;
        if self.length == 0 {
            return Err(invalid(format!("empty FASTA sequence: {name}")));
        }
        self.md5.update(&self.buffer);
        let digest = std::mem::take(&mut self.md5).finalize();
        write!(
            self.output,
            "@SQ\tSN:{name}\tLN:{}\tM5:{digest:x}",
            self.length
        )?;
        if self.options.alias {
            let bare = name.strip_prefix("chr");
            let mitochondrial = bare.unwrap_or(&name);
            if let Some(bare) = bare {
                write!(self.output, "\tAN:{bare}")?;
            } else {
                write!(self.output, "\tAN:chr{name}")?;
            }
            match mitochondrial {
                "M" => write!(self.output, ",chrMT,MT")?,
                "MT" => write!(self.output, ",chrM,M")?,
                _ => {}
            }
        }
        if let Some(uri) = self.uri {
            write!(self.output, "\tUR:{uri}")?;
        }
        if let Some(assembly) = &self.options.assembly {
            write!(self.output, "\tAS:{assembly}")?;
        }
        if let Some(species) = &self.options.species {
            write!(self.output, "\tSP:{species}")?;
        }
        writeln!(self.output)?;
        self.summary.references += 1;
        self.summary.bases = self
            .summary
            .bases
            .checked_add(self.length)
            .ok_or_else(|| invalid("dictionary total length overflow"))?;
        Ok(())
    }
}
