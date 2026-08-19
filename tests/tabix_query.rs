use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rsomics_index::bgzip::{CompressOptions, compress};
use rsomics_index::tabix::{
    BuildOptions, Config, CoordinateSystem, IndexKind, Preset, QueryOptions, build_named, list,
    query,
};

#[test]
fn query_modes_preserve_their_contracts() {
    let directory = tempfile::tempdir().unwrap();
    let data = indexed_vcf(directory.path());

    let ordinary = query_text(
        &data,
        QueryOptions {
            regions: vec!["chr2:1-20".into(), "chr1:1-20".into()],
            ..QueryOptions::default()
        },
    );
    assert!(ordinary.find("chr2\t5").unwrap() < ordinary.find("chr1\t7").unwrap());

    let unique = query_text(
        &data,
        QueryOptions {
            regions: vec!["chr1:1-25".into(), "chr1:10-30".into()],
            unique: true,
            ..QueryOptions::default()
        },
    );
    assert_eq!(unique.matches("chr1\t20").count(), 1);

    let separated = query_text(
        &data,
        QueryOptions {
            regions: vec!["chr1:1-25".into(), "chr1:10-30".into()],
            separate_regions: true,
            ..QueryOptions::default()
        },
    );
    assert!(separated.contains("#chr1:1-25\n"));
    assert!(separated.contains("#chr1:10-30\n"));
    assert_eq!(separated.matches("chr1\t20").count(), 2);
}

#[test]
fn region_and_target_files_preserve_their_distinct_orders() {
    let directory = tempfile::tempdir().unwrap();
    let data = indexed_vcf(directory.path());
    let regions = directory.path().join("query-regions.tsv");
    let targets = directory.path().join("query-targets.bed");
    std::fs::write(&regions, include_bytes!("golden/query-regions.tsv")).unwrap();
    std::fs::write(&targets, include_bytes!("golden/query-targets.bed")).unwrap();

    let region_output = query_text(
        &data,
        QueryOptions {
            regions_file: Some(regions),
            ..QueryOptions::default()
        },
    );
    assert!(region_output.find("chr2\t5").unwrap() < region_output.find("chr1\t7").unwrap());

    let target_output = query_text(
        &data,
        QueryOptions {
            targets_file: Some(targets),
            ..QueryOptions::default()
        },
    );
    assert!(!target_output.contains("chr1\t7"));
    assert!(target_output.find("chr1\t20").unwrap() < target_output.find("chr2\t5").unwrap());

    let combined_targets = directory.path().join("combined-targets.bed");
    std::fs::write(&combined_targets, b"chr1\t13\t40\n").unwrap();
    let combined_output = query_text(
        &data,
        QueryOptions {
            regions: vec!["chr1:1-25".into()],
            targets_file: Some(combined_targets),
            ..QueryOptions::default()
        },
    );
    assert!(!combined_output.contains("chr1\t7"));
    assert_eq!(combined_output.matches("chr1\t20").count(), 1);
}

#[test]
fn header_modes_and_list_use_the_stored_index_contract() {
    let directory = tempfile::tempdir().unwrap();
    let data = indexed_vcf(directory.path());

    let with_header = query_text(
        &data,
        QueryOptions {
            regions: vec!["chr2:1-10".into()],
            print_header: true,
            ..QueryOptions::default()
        },
    );
    assert!(with_header.starts_with("##fileformat=VCFv4.3\n"));
    assert!(with_header.ends_with("chr2\t5\t.\tG\tT\t.\tPASS\t.\n"));

    let header_only = query_text(
        &data,
        QueryOptions {
            header_only: true,
            ..QueryOptions::default()
        },
    );
    assert!(header_only.contains("#CHROM\tPOS"));
    assert!(!header_only.contains("chr1\t7"));

    let mut names = Vec::new();
    let summary = list(&data, &mut names, None).unwrap();
    assert_eq!(summary.references, 2);
    assert_eq!(names, b"chr1\nchr2\n");
}

#[test]
fn query_rejects_conflicts_missing_references_and_write_failures() {
    let directory = tempfile::tempdir().unwrap();
    let data = indexed_vcf(directory.path());

    let mut output = Vec::new();
    let error = query(
        &data,
        &mut output,
        &QueryOptions {
            regions: vec!["chr1:1-20".into()],
            unique: true,
            separate_regions: true,
            ..QueryOptions::default()
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("cannot be used together"),
        "{error}"
    );
    assert!(output.is_empty());

    let error = query(
        &data,
        &mut output,
        &QueryOptions {
            regions: vec!["missing:1-20".into()],
            ..QueryOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("not in index"), "{error}");

    let mut failing = FailingWriter;
    let error = query(
        &data,
        &mut failing,
        &QueryOptions {
            regions: vec!["chr1:1-20".into()],
            ..QueryOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("broken output"), "{error}");

    let error = list(&data, &mut failing, None).unwrap_err();
    assert!(error.to_string().contains("broken output"), "{error}");
}

#[test]
fn open_regions_and_explicit_indexes_are_supported() {
    let directory = tempfile::tempdir().unwrap();
    let data = indexed_vcf(directory.path());
    let default_index = directory.path().join("records.vcf.gz.tbi");
    let explicit_index = directory.path().join("custom.index");
    std::fs::rename(default_index, &explicit_index).unwrap();

    let output = query_text(
        &data,
        QueryOptions {
            regions: vec!["chr1:20-".into(), "chr2".into()],
            index: Some(explicit_index),
            ..QueryOptions::default()
        },
    );

    assert!(!output.contains("chr1\t7"));
    assert!(output.contains("chr1\t20"));
    assert!(output.contains("chr2\t5"));
}

#[test]
fn malformed_region_files_and_stale_indexes_fail_before_output() {
    let directory = tempfile::tempdir().unwrap();
    let data = indexed_vcf(directory.path());
    let malformed = directory.path().join("malformed.bed");
    std::fs::write(&malformed, b"chr1\t10\t10\n").unwrap();
    let mut output = Vec::new();

    let error = query(
        &data,
        &mut output,
        &QueryOptions {
            regions_file: Some(malformed),
            ..QueryOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("greater than start"), "{error}");
    assert!(output.is_empty());

    let index = directory.path().join("records.vcf.gz.tbi");
    let index_modified = std::fs::metadata(index).unwrap().modified().unwrap();
    File::open(&data)
        .unwrap()
        .set_modified(index_modified + Duration::from_secs(2))
        .unwrap();
    let error = query(
        &data,
        &mut output,
        &QueryOptions {
            regions: vec!["chr1:1-10".into()],
            ..QueryOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("older than data"), "{error}");
    assert!(output.is_empty());
}

#[test]
fn truncated_data_and_index_corruption_fail_before_records_are_written() {
    let directory = tempfile::tempdir().unwrap();
    let data = indexed_vcf(directory.path());
    let index = directory.path().join("records.vcf.gz.tbi");
    let mut output = Vec::new();

    let original_data = std::fs::read(&data).unwrap();
    let mut truncated = original_data.clone();
    truncated.pop();
    std::fs::write(&data, truncated).unwrap();
    make_index_fresh(&data, &index);
    let error = query(
        &data,
        &mut output,
        &QueryOptions {
            regions: vec!["chr1:1-10".into()],
            ..QueryOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("EOF marker"), "{error}");
    assert!(output.is_empty());

    let mut corrupt = original_data.clone();
    let first_block_size = usize::from(u16::from_le_bytes([corrupt[16], corrupt[17]])) + 1;
    corrupt[first_block_size - 8] ^= 0xff;
    std::fs::write(&data, corrupt).unwrap();
    make_index_fresh(&data, &index);
    let error = query(
        &data,
        &mut output,
        &QueryOptions {
            regions: vec!["chr1:1-10".into()],
            ..QueryOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("checksum"), "{error}");
    assert!(output.is_empty());

    std::fs::write(&data, original_data).unwrap();
    let mut index_bytes = std::fs::read(&index).unwrap();
    index_bytes.truncate(index_bytes.len() - 1);
    std::fs::write(&index, index_bytes).unwrap();
    File::open(&index)
        .unwrap()
        .set_modified(
            std::fs::metadata(&data).unwrap().modified().unwrap() + Duration::from_secs(2),
        )
        .unwrap();
    let error = query(
        &data,
        &mut output,
        &QueryOptions {
            regions: vec!["chr1:1-10".into()],
            ..QueryOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("truncated"), "{error}");
    assert!(output.is_empty());
}

#[test]
fn stored_custom_columns_drive_header_and_record_queries() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("custom.tsv.gz");
    let output = File::create(&data).unwrap();
    let (output, _) = compress(
        b"preamble\nid1\tchr1\t5\t9\nid2\tchr1\t20\t25\n".as_slice(),
        output,
        &CompressOptions::default(),
    )
    .unwrap();
    drop(output);
    let config =
        Config::custom(2, 3, Some(4), CoordinateSystem::OneBasedInclusive, b'%', 1).unwrap();
    build_named(
        &data,
        &directory.path().join("custom.tsv.gz.tbi"),
        &BuildOptions {
            config,
            kind: IndexKind::Tbi,
        },
    )
    .unwrap();

    let output = query_text(
        &data,
        QueryOptions {
            regions: vec!["chr1:7".into()],
            print_header: true,
            ..QueryOptions::default()
        },
    );

    assert_eq!(output, "id1\tchr1\t5\t9\n");

    let header = query_text(
        &data,
        QueryOptions {
            header_only: true,
            ..QueryOptions::default()
        },
    );
    assert!(header.is_empty());
}

#[test]
fn selected_records_are_reparsed_after_an_index_preserving_data_change() {
    let directory = tempfile::tempdir().unwrap();
    let data = indexed_vcf(directory.path());
    let index = directory.path().join("records.vcf.gz.tbi");
    let malformed = include_bytes!("golden/records.vcf")
        .windows(7)
        .position(|window| window == b"chr1\t7\t")
        .unwrap();
    let mut changed = include_bytes!("golden/records.vcf").to_vec();
    changed[malformed + 5] = b'x';
    let output = File::create(&data).unwrap();
    let (output, _) = compress(changed.as_slice(), output, &CompressOptions::default()).unwrap();
    drop(output);
    make_index_fresh(&data, &index);
    let mut result = Vec::new();

    let error = query(
        &data,
        &mut result,
        &QueryOptions {
            regions: vec!["chr1:1-10".into()],
            ..QueryOptions::default()
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("virtual offset"), "{error}");
    assert!(result.is_empty());
}

#[test]
fn query_reassembles_records_that_span_bgzf_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("long.vcf.gz");
    let mut source = b"##fileformat=VCFv4.3\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\nchr1\t1\t.\tA\tC\t.\tPASS\tX=".to_vec();
    source.extend(std::iter::repeat_n(b'a', 100_000));
    source.push(b'\n');
    let output = File::create(&data).unwrap();
    let (output, _) = compress(source.as_slice(), output, &CompressOptions::default()).unwrap();
    drop(output);
    build_named(
        &data,
        &directory.path().join("long.vcf.gz.tbi"),
        &BuildOptions {
            config: Config::from_preset(Preset::Vcf),
            kind: IndexKind::Tbi,
        },
    )
    .unwrap();
    let mut queried = Vec::new();

    let summary = query(
        &data,
        &mut queried,
        &QueryOptions {
            regions: vec!["chr1:1".into()],
            workers: std::num::NonZero::new(4).unwrap(),
            cache_bytes: 0,
            ..QueryOptions::default()
        },
    )
    .unwrap();

    let record_start = source
        .windows(b"chr1\t1\t".len())
        .position(|window| window == b"chr1\t1\t")
        .unwrap();
    assert_eq!(summary.records, 1);
    assert_eq!(queried, source[record_start..]);
    assert!(queried.starts_with(b"chr1\t1\t"));
}

fn indexed_vcf(directory: &Path) -> PathBuf {
    let data = directory.join("records.vcf.gz");
    let output = File::create(&data).unwrap();
    let (output, _) = compress(
        include_bytes!("golden/records.vcf").as_slice(),
        output,
        &CompressOptions::default(),
    )
    .unwrap();
    drop(output);
    let index = directory.join("records.vcf.gz.tbi");
    build_named(
        &data,
        &index,
        &BuildOptions {
            config: Config::from_preset(Preset::Vcf),
            kind: IndexKind::Tbi,
        },
    )
    .unwrap();
    data
}

fn query_text(data: &Path, options: QueryOptions) -> String {
    let mut output = Vec::new();
    query(data, &mut output, &options).unwrap();
    String::from_utf8(output).unwrap()
}

fn make_index_fresh(data: &Path, index: &Path) {
    let data_modified = std::fs::metadata(data).unwrap().modified().unwrap();
    File::open(index)
        .unwrap()
        .set_modified(data_modified + Duration::from_secs(2))
        .unwrap();
}

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken output"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
