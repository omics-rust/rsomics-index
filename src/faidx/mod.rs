mod index;
mod query;
mod reader;
mod region;

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rsomics_common::{
    AtomicFile, Context, Result, RsomicsError, reject_output_alias, write_output,
};
use serde::Serialize;

use crate::bgzip::GziIndex;
use crate::output::{ensure_replaceable, named_path, sidecar_path};
use crate::reference::{scan, small_line};

#[derive(Debug, Serialize)]
pub(crate) struct BuildSummary {
    references: usize,
    bases: u64,
    compressed: bool,
    gzi_entries: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct QuerySummary {
    regions: usize,
    bases: u64,
    workers: usize,
}

pub(crate) fn build(
    reference: &Path,
    fai: Option<&Path>,
    gzi: Option<&Path>,
    force: bool,
) -> Result<BuildSummary> {
    if reference == Path::new("-") {
        return Err(config("FASTA indexing requires a named seekable reference"));
    }
    let mut source =
        reader::Source::open(reference, None, true).rs_context("opening FASTA reference")?;
    let compressed = source.compressed();
    if !compressed && gzi.is_some() {
        return Err(config("--gzi-idx is valid only for BGZF references"));
    }
    let fai = fai.map_or_else(|| sidecar_path(reference, "fai"), Path::to_path_buf);
    let gzi =
        compressed.then(|| gzi.map_or_else(|| sidecar_path(reference, "gzi"), Path::to_path_buf));
    for path in std::iter::once(&fai).chain(gzi.as_ref()) {
        if path == Path::new("-") {
            return Err(config("FASTA indexes require named output paths"));
        }
        reject_output_alias(path, [reference])?;
        ensure_replaceable(path, force)?;
    }
    if let Some(gzi) = &gzi {
        reject_output_alias(gzi, [fai.as_path()])?;
    }
    let mut builder = index::Builder::default();
    scan(
        &mut BufReader::with_capacity(256 * 1024, &mut source),
        &mut builder,
    )
    .rs_context("indexing FASTA reference")?;
    let index = source.scanned_index();
    let mut staged = AtomicFile::new(&fai)?;
    builder.index.write(staged.file_mut())?;
    let mut outputs = vec![staged];
    if let (Some(path), Some(index)) = (gzi, &index) {
        let mut staged = AtomicFile::new(path)?;
        index.write(staged.file_mut())?;
        outputs.push(staged);
    }
    AtomicFile::commit_all(outputs)?;
    Ok(BuildSummary {
        references: builder.index.records.len(),
        bases: builder
            .index
            .records
            .iter()
            .map(|record| record.length)
            .sum(),
        compressed,
        gzi_entries: index.as_ref().map(|index| index.entries().len()),
    })
}

pub(crate) struct QueryOptions {
    pub(crate) reference: PathBuf,
    pub(crate) fai: Option<PathBuf>,
    pub(crate) gzi: Option<PathBuf>,
    pub(crate) regions: Vec<String>,
    pub(crate) region_file: Option<PathBuf>,
    pub(crate) output: Option<PathBuf>,
    pub(crate) width: Option<u64>,
    pub(crate) workers: usize,
    pub(crate) force: bool,
}

pub(crate) fn query(options: QueryOptions) -> Result<QuerySummary> {
    if options.reference == Path::new("-") {
        return Err(config(
            "FASTA random access requires a named seekable reference",
        ));
    }
    let source = reader::Source::open(&options.reference, None, false)?;
    let compressed = source.compressed();
    if !compressed && options.gzi.is_some() {
        return Err(config("--gzi-idx is valid only for BGZF references"));
    }
    let fai = options
        .fai
        .unwrap_or_else(|| sidecar_path(&options.reference, "fai"));
    let gzi_path = compressed.then(|| {
        options
            .gzi
            .unwrap_or_else(|| sidecar_path(&options.reference, "gzi"))
    });
    let index = Arc::new(index::Index::read(&mut BufReader::new(
        File::open(&fai).rs_context("opening existing FAI index; run faidx build first")?,
    ))?);
    let gzi = gzi_path
        .as_ref()
        .map(|path| {
            File::open(path)
                .and_then(GziIndex::read_reference)
                .map(Arc::new)
                .map_err(RsomicsError::Io)
        })
        .transpose()?;
    let mut requests = Vec::new();
    if let Some(path) = &options.region_file {
        let mut file = BufReader::new(File::open(path)?);
        loop {
            let line = small_line(&mut file)?;
            if line.is_empty() {
                break;
            }
            requests.push(
                std::str::from_utf8(&line)?
                    .trim_end_matches(['\r', '\n'])
                    .to_owned(),
            );
        }
    }
    requests.extend(options.regions);
    if requests.is_empty() {
        return Err(config(
            "faidx query requires regions or a nonempty --region-file",
        ));
    }
    let regions = requests
        .into_iter()
        .map(|request| region::resolve(&index, request))
        .collect::<Result<Vec<_>>>()?;
    if let Some(output) = named_path(options.output.as_deref()) {
        let mut inputs = vec![options.reference.as_path(), fai.as_path()];
        inputs.extend(gzi_path.as_deref());
        inputs.extend(options.region_file.as_deref());
        reject_output_alias(output, inputs)?;
        ensure_replaceable(output, options.force)?;
    }
    let bases = write_output(options.output.as_deref(), |output| {
        query::run(
            &options.reference,
            index,
            gzi,
            &regions,
            options.workers,
            options.width,
            output,
        )
    })?;
    Ok(QuerySummary {
        regions: regions.len(),
        bases,
        workers: options.workers,
    })
}

fn config(message: &str) -> RsomicsError {
    RsomicsError::ConfigError(message.to_owned())
}
