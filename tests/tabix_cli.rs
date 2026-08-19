use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use rsomics_index::bgzip::{CompressOptions, compress, decompress};
use rsomics_index::tabix::{
    BuildOptions, Config, IndexKind, Preset, QueryOptions, build, build_named, list, query,
};

#[test]
fn builds_queryable_tbi_and_csi() {
    let directory = tempfile::tempdir().unwrap();
    let input = bgzip_fixture(
        directory.path(),
        "records.vcf.gz",
        include_bytes!("golden/records.vcf"),
    );

    for kind in [IndexKind::Tbi, IndexKind::Csi { min_shift: 14 }] {
        let output = directory.path().join(match kind {
            IndexKind::Tbi => "records.vcf.gz.tbi",
            IndexKind::Csi { .. } => "records.vcf.gz.csi",
        });
        let mut file = File::create(&output).unwrap();
        let summary = build(
            &input,
            &mut file,
            &BuildOptions {
                config: Config::from_preset(Preset::Vcf),
                kind,
            },
        )
        .unwrap();
        drop(file);
        let mut references = Vec::new();
        let listed = list(&input, &mut references, Some(&output)).unwrap();
        let mut records = Vec::new();
        let queried = query(
            &input,
            &mut records,
            &QueryOptions {
                regions: vec!["chr1:1-20".into()],
                index: Some(output),
                ..QueryOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.records, 3);
        assert_eq!(summary.references, 2);
        assert_eq!(listed.references, 2);
        assert_eq!(references, b"chr1\nchr2\n");
        assert!(queried.records > 0);
        assert!(!records.is_empty());
    }
}

#[test]
fn malformed_late_record_does_not_replace_index() {
    let directory = tempfile::tempdir().unwrap();
    let input = bgzip_fixture(
        directory.path(),
        "unsorted.bed.gz",
        b"chr1\t0\t10\nchr1\t20\t30\nchr1\t15\t25\n",
    );
    let output = directory.path().join("unsorted.bed.gz.tbi");
    std::fs::write(&output, b"existing index").unwrap();
    let options = BuildOptions {
        config: Config::from_preset(Preset::Bed),
        kind: IndexKind::Tbi,
    };

    let error = build_named(&input, &output, &options).unwrap_err();

    assert!(error.to_string().contains("sorted"), "{error}");
    assert_eq!(std::fs::read(output).unwrap(), b"existing index");
}

#[test]
fn tbi_accepts_its_last_base_and_rejects_the_next() {
    let directory = tempfile::tempdir().unwrap();
    let options = BuildOptions {
        config: Config::from_preset(Preset::Bed),
        kind: IndexKind::Tbi,
    };
    let valid = bgzip_fixture(
        directory.path(),
        "boundary.bed.gz",
        b"chr1\t536870911\t536870912\n",
    );
    let mut valid_output = File::create(directory.path().join("boundary.bed.gz.tbi")).unwrap();

    build(&valid, &mut valid_output, &options).unwrap();

    let invalid = bgzip_fixture(
        directory.path(),
        "past-boundary.bed.gz",
        b"chr1\t536870912\t536870913\n",
    );
    let mut invalid_output =
        File::create(directory.path().join("past-boundary.bed.gz.tbi")).unwrap();
    let error = build(&invalid, &mut invalid_output, &options).unwrap_err();
    assert!(
        error.to_string().contains("TBI coordinate limit"),
        "{error}"
    );
}

#[test]
fn truncated_bgzf_does_not_replace_index() {
    let directory = tempfile::tempdir().unwrap();
    let input = bgzip_fixture(directory.path(), "truncated.bed.gz", b"chr1\t0\t10\n");
    let mut bytes = std::fs::read(&input).unwrap();
    bytes.truncate(bytes.len() - 1);
    std::fs::write(&input, bytes).unwrap();
    let output = directory.path().join("truncated.bed.gz.tbi");
    std::fs::write(&output, b"existing index").unwrap();

    let error = build_named(
        &input,
        &output,
        &BuildOptions {
            config: Config::from_preset(Preset::Bed),
            kind: IndexKind::Tbi,
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("EOF"), "{error}");
    assert_eq!(std::fs::read(output).unwrap(), b"existing index");
}

#[test]
fn index_loader_rejects_decompressed_trailing_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let input = bgzip_fixture(directory.path(), "records.bed.gz", b"chr1\t0\t10\n");
    let index = directory.path().join("records.bed.gz.tbi");
    build_named(
        &input,
        &index,
        &BuildOptions {
            config: Config::from_preset(Preset::Bed),
            kind: IndexKind::Tbi,
        },
    )
    .unwrap();
    let mut references = Vec::new();
    assert_eq!(list(&input, &mut references, None).unwrap().references, 1);

    let mut raw = Vec::new();
    decompress(Cursor::new(std::fs::read(&index).unwrap()), &mut raw, None).unwrap();
    raw.extend_from_slice(b"trailing");
    let options = CompressOptions {
        text: false,
        ..CompressOptions::default()
    };
    let (compressed, _) = compress(raw.as_slice(), Vec::new(), &options).unwrap();
    std::fs::write(&index, compressed).unwrap();

    let Err(error) = list(&input, &mut Vec::new(), Some(&index)) else {
        panic!("index with trailing bytes was accepted");
    };
    assert!(error.to_string().contains("trailing"), "{error}");
}

#[test]
fn index_loader_rejects_truncation_and_post_eof_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let input = bgzip_fixture(directory.path(), "records.gff.gz", b"chr1\ts\tg\t1\t9\n");
    let index = directory.path().join("records.gff.gz.tbi");
    build_named(
        &input,
        &index,
        &BuildOptions {
            config: Config::from_preset(Preset::Gff),
            kind: IndexKind::Tbi,
        },
    )
    .unwrap();
    let bytes = std::fs::read(&index).unwrap();

    let mut truncated = bytes.clone();
    truncated.pop();
    let truncated_path = directory.path().join("truncated.tbi");
    std::fs::write(&truncated_path, truncated).unwrap();
    assert!(list(&input, &mut Vec::new(), Some(&truncated_path)).is_err());

    let mut post_eof = bytes;
    post_eof.push(0);
    let post_eof_path = directory.path().join("post-eof.tbi");
    std::fs::write(&post_eof_path, post_eof).unwrap();
    let Err(error) = list(&input, &mut Vec::new(), Some(&post_eof_path)) else {
        panic!("index with post-EOF bytes was accepted");
    };
    assert!(error.to_string().contains("EOF marker"), "{error}");
}

#[test]
fn empty_and_header_only_inputs_produce_empty_indexes() {
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        ("empty.bed.gz", b"".as_slice(), Preset::Bed),
        (
            "header.vcf.gz",
            b"##fileformat=VCFv4.3\n##contig=<ID=chr1,length=1000>\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n".as_slice(),
            Preset::Vcf,
        ),
    ];

    for (name, data, preset) in cases {
        let input = bgzip_fixture(directory.path(), name, data);
        for kind in [IndexKind::Tbi, IndexKind::Csi { min_shift: 14 }] {
            let output = directory.path().join(format!("{name}.{kind:?}"));
            let mut file = File::create(&output).unwrap();
            let summary = build(
                &input,
                &mut file,
                &BuildOptions {
                    config: Config::from_preset(preset),
                    kind,
                },
            )
            .unwrap();
            drop(file);
            let mut names = Vec::new();
            let listed = list(&input, &mut names, Some(&output)).unwrap();
            assert_eq!(summary.records, 0);
            assert_eq!(summary.references, 0);
            assert_eq!(listed.references, 0);
            assert!(names.is_empty());
        }
    }
}

#[test]
fn csi_minimum_shift_is_checked_before_output_is_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let input = bgzip_fixture(directory.path(), "records.bed.gz", b"chr1\t0\t10\n");

    for min_shift in [0, 32] {
        let output = directory.path().join(format!("invalid-{min_shift}.csi"));
        std::fs::write(&output, b"existing index").unwrap();
        let error = build_named(
            &input,
            &output,
            &BuildOptions {
                config: Config::from_preset(Preset::Bed),
                kind: IndexKind::Csi { min_shift },
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("minimum shift"), "{error}");
        assert_eq!(std::fs::read(output).unwrap(), b"existing index");
    }
}

fn bgzip_fixture(directory: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = directory.join(name);
    let output = File::create(&path).unwrap();
    let (output, _) = compress(data, output, &CompressOptions::default()).unwrap();
    drop(output);
    path
}
