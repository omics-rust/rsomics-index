mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use noodles::{csi, tabix};

use support::{Oracle, assert_success, ours};

#[test]
#[ignore = "requires HTSlib 1.24 oracles"]
fn tbi_structures_and_queries_match_for_every_preset() {
    let oracle = Oracle::require();
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        ("bed", include_bytes!("golden/records.bed").as_slice()),
        ("gff", include_bytes!("golden/records.gff").as_slice()),
        ("sam", include_bytes!("golden/records.sam").as_slice()),
        ("vcf", include_bytes!("golden/records.vcf").as_slice()),
    ];

    for (preset, source) in cases {
        let data = directory.path().join(format!("{preset}.gz"));
        compress(source, &data);
        let ours_index = build_ours_tbi(&data, preset, directory.path());
        let hts_index = build_hts_tbi(&oracle, &data, preset);

        let ours_structure = tabix::fs::read(&ours_index).unwrap();
        let hts_structure = tabix::fs::read(&hts_index).unwrap();
        assert_eq!(ours_structure, hts_structure, "{preset} TBI structure");

        let ours_output = ours_query(&data, &hts_index, &["chr1:1-12"]);
        let hts_output = hts_query(&oracle, &data, &["chr1:1-12"]);
        assert_eq!(ours_output.stdout, hts_output.stdout, "{preset} query");

        std::fs::copy(&ours_index, &hts_index).unwrap();
        let hts_from_ours = hts_query(&oracle, &data, &["chr1:1-12"]);
        assert_eq!(
            ours_output.stdout, hts_from_ours.stdout,
            "{preset} cross-read"
        );
    }
}

#[test]
#[ignore = "requires HTSlib 1.24 oracles"]
fn csi_structures_and_queries_match_at_multiple_minimum_shifts() {
    let oracle = Oracle::require();
    let directory = tempfile::tempdir().unwrap();

    for min_shift in [10, 14] {
        let data = directory.path().join(format!("shift-{min_shift}.vcf.gz"));
        compress(include_bytes!("golden/records.vcf"), &data);
        let default_index = sidecar(&data, "csi");
        let ours_index = directory.path().join(format!("shift-{min_shift}.ours.csi"));
        let built = Command::new(ours())
            .args(["tabix", "build", "--preset", "vcf", "--csi", "--min-shift"])
            .arg(min_shift.to_string())
            .arg(&data)
            .output()
            .unwrap();
        assert_success(&built);
        std::fs::rename(&default_index, &ours_index).unwrap();

        let built = Command::new(oracle.program("tabix"))
            .args(["--force", "--preset", "vcf", "--csi", "--min-shift"])
            .arg(min_shift.to_string())
            .arg(&data)
            .output()
            .unwrap();
        assert_success(&built);

        let ours_structure = csi::fs::read(&ours_index).unwrap();
        let hts_structure = csi::fs::read(&default_index).unwrap();
        assert_eq!(ours_structure, hts_structure, "CSI min_shift={min_shift}");

        let ours_output = ours_query(&data, &default_index, &["chr1:1-30"]);
        let hts_output = hts_query(&oracle, &data, &["chr1:1-30"]);
        assert_eq!(ours_output.stdout, hts_output.stdout);

        std::fs::copy(&ours_index, &default_index).unwrap();
        let hts_from_ours = hts_query(&oracle, &data, &["chr1:1-30"]);
        assert_eq!(ours_output.stdout, hts_from_ours.stdout);
    }
}

#[test]
#[ignore = "requires HTSlib 1.24 oracles"]
fn custom_columns_headers_and_query_modes_match() {
    let oracle = Oracle::require();
    let directory = tempfile::tempdir().unwrap();
    let custom = b"preamble\nid1\tchr1\t5\t9\nid2\tchr1\t20\t25\n";
    let data = directory.path().join("custom.tsv.gz");
    compress(custom, &data);
    let default_index = sidecar(&data, "tbi");
    let ours_index = directory.path().join("custom.ours.tbi");

    let built = Command::new(ours())
        .args([
            "tabix",
            "build",
            "--sequence-column",
            "2",
            "--begin-column",
            "3",
            "--end-column",
            "4",
            "--skip-lines",
            "1",
            "--comment",
            "%",
        ])
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&built);
    std::fs::rename(&default_index, &ours_index).unwrap();

    let built = Command::new(oracle.program("tabix"))
        .args([
            "--force", "-s", "2", "-b", "3", "-e", "4", "-S", "1", "-c", "%",
        ])
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&built);
    assert_eq!(
        tabix::fs::read(&ours_index).unwrap(),
        tabix::fs::read(&default_index).unwrap()
    );

    let ours_output = ours_query(&data, &default_index, &["chr1:7-7"]);
    let hts_output = hts_query(&oracle, &data, &["chr1:7-7"]);
    assert_eq!(ours_output.stdout, hts_output.stdout);

    let ours_header = Command::new(ours())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("--index")
        .arg(&default_index)
        .arg("--header-only")
        .output()
        .unwrap();
    assert_success(&ours_header);
    let hts_header = Command::new(oracle.program("tabix"))
        .arg("--only-header")
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&hts_header);
    assert_eq!(ours_header.stdout, hts_header.stdout);
}

#[test]
#[ignore = "requires HTSlib 1.24 oracles"]
fn vcf_region_target_header_deduplication_and_list_modes_match() {
    let oracle = Oracle::require();
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("records.vcf.gz");
    let regions_file = directory.path().join("regions.tsv");
    let targets_file = directory.path().join("targets.bed");
    compress(include_bytes!("golden/records.vcf"), &data);
    std::fs::write(&regions_file, include_bytes!("golden/query-regions.tsv")).unwrap();
    std::fs::write(&targets_file, include_bytes!("golden/query-targets.bed")).unwrap();
    let built = Command::new(oracle.program("tabix"))
        .args(["--preset", "vcf"])
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&built);
    let index = sidecar(&data, "tbi");

    compare_queries(
        ours_query(&data, &index, &["chr2:1-10", "chr1:1-25"]),
        hts_query(&oracle, &data, &["chr2:1-10", "chr1:1-25"]),
        "ordered inline regions",
    );

    let ours_header = Command::new(ours())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("--index")
        .arg(&index)
        .arg("--print-header")
        .arg("chr2:1-10")
        .output()
        .unwrap();
    let hts_header = Command::new(oracle.program("tabix"))
        .arg("--print-header")
        .arg(&data)
        .arg("chr2:1-10")
        .output()
        .unwrap();
    compare_queries(ours_header, hts_header, "header inclusion");

    let ours_only_header = Command::new(ours())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("--index")
        .arg(&index)
        .arg("--header-only")
        .output()
        .unwrap();
    let hts_only_header = Command::new(oracle.program("tabix"))
        .arg("--only-header")
        .arg(&data)
        .output()
        .unwrap();
    compare_queries(ours_only_header, hts_only_header, "header only");

    let ours_unique = Command::new(ours())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("--index")
        .arg(&index)
        .arg("--unique")
        .args(["chr1:1-25", "chr1:10-30"])
        .output()
        .unwrap();
    let hts_unique = Command::new(oracle.program("tabix"))
        .arg("--unique")
        .arg(&data)
        .args(["chr1:1-25", "chr1:10-30"])
        .output()
        .unwrap();
    compare_queries(ours_unique, hts_unique, "unique overlap");

    let ours_separate = Command::new(ours())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("--index")
        .arg(&index)
        .arg("--separate-regions")
        .args(["chr1:1-25", "chr1:10-30"])
        .output()
        .unwrap();
    let hts_separate = Command::new(oracle.program("tabix"))
        .arg("--separate-regions")
        .arg(&data)
        .args(["chr1:1-25", "chr1:10-30"])
        .output()
        .unwrap();
    compare_queries(ours_separate, hts_separate, "region separators");

    let ours_regions = Command::new(ours())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("--index")
        .arg(&index)
        .arg("--regions-file")
        .arg(&regions_file)
        .output()
        .unwrap();
    let hts_regions = Command::new(oracle.program("tabix"))
        .arg("--regions")
        .arg(&regions_file)
        .arg(&data)
        .output()
        .unwrap();
    compare_queries(ours_regions, hts_regions, "regions file");

    let ours_targets = Command::new(ours())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("--index")
        .arg(&index)
        .arg("--targets-file")
        .arg(&targets_file)
        .arg("--threads")
        .arg("2")
        .arg("--cache-bytes")
        .arg("0")
        .output()
        .unwrap();
    let hts_targets = Command::new(oracle.program("tabix"))
        .arg("--targets")
        .arg(&targets_file)
        .arg("--threads")
        .arg("2")
        .arg("--cache")
        .arg("0")
        .arg(&data)
        .output()
        .unwrap();
    compare_queries(ours_targets, hts_targets, "targets file");

    let ours_list = Command::new(ours())
        .args(["tabix", "list"])
        .arg(&data)
        .arg("--index")
        .arg(&index)
        .output()
        .unwrap();
    let hts_list = Command::new(oracle.program("tabix"))
        .arg("--list-chroms")
        .arg(&data)
        .output()
        .unwrap();
    compare_queries(ours_list, hts_list, "reference list");
}

#[test]
#[ignore = "requires HTSlib 1.24 oracles"]
fn both_builders_reject_unsorted_data() {
    let oracle = Oracle::require();
    let directory = tempfile::tempdir().unwrap();
    let source = b"chr1\t0\t10\nchr1\t20\t30\nchr1\t15\t25\n";
    let ours_data = directory.path().join("ours-unsorted.bed.gz");
    let hts_data = directory.path().join("hts-unsorted.bed.gz");
    compress(source, &ours_data);
    std::fs::copy(&ours_data, &hts_data).unwrap();

    let ours_result = Command::new(ours())
        .args(["tabix", "build", "--preset", "bed"])
        .arg(&ours_data)
        .output()
        .unwrap();
    let hts_result = Command::new(oracle.program("tabix"))
        .args(["--preset", "bed"])
        .arg(&hts_data)
        .output()
        .unwrap();
    assert!(!ours_result.status.success());
    assert!(!hts_result.status.success());
    assert!(!sidecar(&ours_data, "tbi").exists());
    assert!(!sidecar(&hts_data, "tbi").exists());
}

fn compress(source: &[u8], output: &Path) {
    let input = output.with_extension("source");
    std::fs::write(&input, source).unwrap();
    let result = Command::new(ours())
        .args(["bgzip"])
        .arg(&input)
        .arg("--output")
        .arg(output)
        .output()
        .unwrap();
    assert_success(&result);
}

fn build_ours_tbi(data: &Path, preset: &str, directory: &Path) -> PathBuf {
    let default_index = sidecar(data, "tbi");
    let result = Command::new(ours())
        .args(["tabix", "build", "--preset", preset])
        .arg(data)
        .output()
        .unwrap();
    assert_success(&result);
    let output = directory.join(format!("{preset}.ours.tbi"));
    std::fs::rename(default_index, &output).unwrap();
    output
}

fn build_hts_tbi(oracle: &Oracle, data: &Path, preset: &str) -> PathBuf {
    let result = Command::new(oracle.program("tabix"))
        .args(["--force", "--preset", preset])
        .arg(data)
        .output()
        .unwrap();
    assert_success(&result);
    sidecar(data, "tbi")
}

fn ours_query(data: &Path, index: &Path, regions: &[&str]) -> Output {
    let output = Command::new(ours())
        .args(["tabix", "query"])
        .arg(data)
        .arg("--index")
        .arg(index)
        .args(regions)
        .output()
        .unwrap();
    assert_success(&output);
    output
}

fn hts_query(oracle: &Oracle, data: &Path, regions: &[&str]) -> Output {
    let output = Command::new(oracle.program("tabix"))
        .arg(data)
        .args(regions)
        .output()
        .unwrap();
    assert_success(&output);
    output
}

fn compare_queries(ours_output: Output, hts_output: Output, case: &str) {
    assert_success(&ours_output);
    assert_success(&hts_output);
    assert_eq!(ours_output.stdout, hts_output.stdout, "{case}");
}

fn sidecar(data: &Path, extension: &str) -> PathBuf {
    let mut value = data.as_os_str().to_os_string();
    value.push(".");
    value.push(extension);
    PathBuf::from(value)
}
