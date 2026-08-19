use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use rsomics_index::bgzip::{CompressOptions, compress, decompress};
use rsomics_index::tabix::{BuildOptions, IndexKind, LoadedIndex, build, build_named, load_index};
use rsomics_index::tabix::{Config, CoordinateSystem, Preset, Record, SortedState};

#[test]
fn presets_share_a_checked_coordinate_model() {
    let bed = Config::from_preset(Preset::Bed)
        .parse(b"chr1\t0\t10", 1)
        .unwrap();
    let gff = Config::from_preset(Preset::Gff)
        .parse(b"chr1\tsrc\tgene\t1\t10\t.\t+\t.\tID=g", 1)
        .unwrap();

    assert_eq!((bed.start, bed.end), (1, 10));
    assert_eq!((gff.start, gff.end), (1, 10));
}

#[test]
fn sorted_state_rejects_reference_reentry() {
    let mut state = SortedState::default();
    state.push(&record(b"chr1", 1, 2), 1).unwrap();
    state.push(&record(b"chr2", 1, 2), 2).unwrap();

    let error = state.push(&record(b"chr1", 3, 4), 3).unwrap_err();

    assert!(error.to_string().contains("reappears"), "{error}");
}

#[test]
fn sam_and_vcf_presets_derive_formatted_ends() {
    let sam = Config::from_preset(Preset::Sam)
        .parse(
            b"read1\t0\tchr1\t1\t60\t5M2D3M\t*\t0\t0\tACGTACGT\tFFFFFFFF",
            4,
        )
        .unwrap();
    let vcf_ref = Config::from_preset(Preset::Vcf)
        .parse(b"chr1\t7\t.\tAC\tA\t.\tPASS\t.", 4)
        .unwrap();
    let vcf_end = Config::from_preset(Preset::Vcf)
        .parse(b"chr1\t20\t.\tN\t<DEL>\t.\tPASS\tEND=35", 5)
        .unwrap();

    assert_eq!((sam.start, sam.end), (1, 10));
    assert_eq!((vcf_ref.start, vcf_ref.end), (7, 8));
    assert_eq!((vcf_end.start, vcf_end.end), (20, 35));
}

#[test]
fn custom_columns_and_crlf_use_the_selected_coordinate_system() {
    let one_based =
        Config::custom(2, 3, Some(4), CoordinateSystem::OneBasedInclusive, b'#', 1).unwrap();
    let zero_based =
        Config::custom(1, 2, Some(3), CoordinateSystem::ZeroBasedHalfOpen, b'#', 0).unwrap();

    let one = one_based.parse(b"id\tchr3\t4\t9\r\n", 2).unwrap();
    let zero = zero_based.parse(b"chr3\t0\t9\r\n", 1).unwrap();

    assert_eq!(
        (one.reference, one.start, one.end),
        (b"chr3".as_slice(), 4, 9)
    );
    assert_eq!((zero.start, zero.end), (1, 9));
    assert!(one_based.is_meta(1, b"not a comment"));
    assert!(one_based.is_meta(2, b"# comment"));
}

#[test]
fn malformed_coordinates_and_cigar_fail_with_line_context() {
    let cases = [
        (Preset::Gff, b"chr1\ts\tg\t0\t10\t.\t+\t.\t.".as_slice()),
        (Preset::Gff, b"chr1\ts\tg\t10\t9\t.\t+\t.\t.".as_slice()),
        (Preset::Bed, b"chr1\t0".as_slice()),
        (Preset::Bed, b"chr1\t10\t10".as_slice()),
        (
            Preset::Sam,
            b"r\t0\tchr1\t1\t60\t10M5\t*\t0\t0\tA\tF".as_slice(),
        ),
        (
            Preset::Sam,
            b"r\t0\tchr1\t1\t60\t10Q\t*\t0\t0\tA\tF".as_slice(),
        ),
        (Preset::Vcf, b"chr1\t1\t.\t.\tA\t.\tPASS\t.".as_slice()),
        (Preset::Vcf, b"chr1\t10\t.\tA\tT\t.\tPASS\tEND=9".as_slice()),
    ];

    for (preset, line) in cases {
        let error = Config::from_preset(preset).parse(line, 17).unwrap_err();
        assert!(error.to_string().contains("line 17"), "{preset:?}: {error}");
    }
}

#[test]
fn coordinate_overflow_is_rejected() {
    let error = Config::from_preset(Preset::Gff)
        .parse(b"chr1\ts\tg\t18446744073709551616\t20\t.\t+\t.\t.", 3)
        .unwrap_err();

    assert!(error.to_string().contains("line 3"), "{error}");
}

#[test]
fn reference_names_cannot_contain_the_tbi_nul_delimiter() {
    let error = Config::from_preset(Preset::Bed)
        .parse(b"chr\x001\t0\t10", 4)
        .unwrap_err();

    assert!(error.to_string().contains("reference"), "{error}");
    assert!(error.to_string().contains("line 4"), "{error}");
}

#[test]
fn vcf_duplicate_end_fields_are_rejected_even_when_one_is_missing() {
    let error = Config::from_preset(Preset::Vcf)
        .parse(b"chr1\t10\t.\tA\t<DEL>\t.\tPASS\tEND=.;END=20", 6)
        .unwrap_err();

    assert!(error.to_string().contains("duplicate END"), "{error}");
}

#[test]
fn detection_uses_format_evidence_and_rejects_ambiguity() {
    let fixtures = [
        (
            "records.bed.gz",
            include_bytes!("golden/records.bed").as_slice(),
            Preset::Bed,
        ),
        (
            "records.gff3",
            include_bytes!("golden/records.gff").as_slice(),
            Preset::Gff,
        ),
        (
            "records.sam",
            include_bytes!("golden/records.sam").as_slice(),
            Preset::Sam,
        ),
        (
            "records.vcf.bgz",
            include_bytes!("golden/records.vcf").as_slice(),
            Preset::Vcf,
        ),
    ];

    for (name, sample, expected) in fixtures {
        let config = Config::detect(Some(Path::new(name)), sample).unwrap();
        assert_eq!(config.preset(), Some(expected));
    }

    let ambiguous = b"read1\t0\tchr1\t1\t60\t5M\t*\t0\t0\tA\tF";
    let error = Config::detect(None, ambiguous).unwrap_err();
    assert!(error.to_string().contains("--preset"), "{error}");

    assert_eq!(
        Config::detect(Some(Path::new("empty.bed")), b"")
            .unwrap()
            .preset(),
        Some(Preset::Bed)
    );
    assert!(Config::detect(None, b"").is_err());

    let header_only = b"##fileformat=VCFv4.3\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n";
    assert_eq!(
        Config::detect(None, header_only).unwrap().preset(),
        Some(Preset::Vcf)
    );
}

#[test]
fn detection_rejects_conflicting_header_and_extension() {
    let error = Config::detect(
        Some(Path::new("calls.bed")),
        include_bytes!("golden/records.vcf"),
    )
    .unwrap_err();

    assert!(error.to_string().contains("conflict"), "{error}");
}

#[test]
fn sorted_state_rejects_position_regression() {
    let mut state = SortedState::default();
    state.push(&record(b"chr1", 10, 12), 8).unwrap();

    let error = state.push(&record(b"chr1", 9, 11), 9).unwrap_err();

    assert!(error.to_string().contains("line 9"), "{error}");
    assert!(error.to_string().contains("sorted"), "{error}");
}

#[test]
fn all_fixture_records_parse_in_sorted_order() {
    let fixtures = [
        (Preset::Bed, include_bytes!("golden/records.bed").as_slice()),
        (Preset::Gff, include_bytes!("golden/records.gff").as_slice()),
        (Preset::Sam, include_bytes!("golden/records.sam").as_slice()),
        (Preset::Vcf, include_bytes!("golden/records.vcf").as_slice()),
    ];

    for (preset, fixture) in fixtures {
        let config = Config::from_preset(preset);
        let mut sorted = SortedState::default();
        for (index, line) in fixture.split(|byte| *byte == b'\n').enumerate() {
            let line_no = index as u64 + 1;
            if line.is_empty() || config.is_meta(line_no, line) {
                continue;
            }
            let record = config.parse(line, line_no).unwrap();
            sorted.push(&record, line_no).unwrap();
        }
    }
}

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
        let bytes = std::fs::read(output).unwrap();
        let loaded = LoadedIndex::read(Cursor::new(bytes)).unwrap();

        assert_eq!(summary.records, 3);
        assert_eq!(summary.references, 2);
        assert_eq!(
            loaded.reference_names(),
            vec![b"chr1".as_slice(), b"chr2".as_slice()]
        );
        assert!(!loaded.query(0, 1..=20).unwrap().is_empty());
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
    assert_eq!(load_index(&input, None).unwrap().kind(), IndexKind::Tbi);

    let mut raw = Vec::new();
    decompress(Cursor::new(std::fs::read(index).unwrap()), &mut raw, None).unwrap();
    raw.extend_from_slice(b"trailing");
    let options = CompressOptions {
        text: false,
        ..CompressOptions::default()
    };
    let (compressed, _) = compress(raw.as_slice(), Vec::new(), &options).unwrap();

    let Err(error) = LoadedIndex::read(Cursor::new(compressed)) else {
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
    let bytes = std::fs::read(index).unwrap();

    let mut truncated = bytes.clone();
    truncated.pop();
    assert!(LoadedIndex::read(Cursor::new(truncated)).is_err());

    let mut post_eof = bytes;
    post_eof.push(0);
    let Err(error) = LoadedIndex::read(Cursor::new(post_eof)) else {
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
            let loaded = LoadedIndex::read(File::open(output).unwrap()).unwrap();
            assert_eq!(summary.records, 0);
            assert_eq!(summary.references, 0);
            assert!(loaded.reference_names().is_empty());
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

fn record(reference: &[u8], start: u64, end: u64) -> Record<'_> {
    Record {
        reference,
        start,
        end,
    }
}
