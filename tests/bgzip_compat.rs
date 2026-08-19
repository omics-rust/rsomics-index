mod support;

use std::fs::File;
use std::path::Path;
use std::process::Command;

use rsomics_index::bgzip::GziIndex;

use support::{Oracle, assert_success, ours};

#[test]
#[ignore = "requires HTSlib 1.24 oracles"]
fn cross_tool_bgzf_round_trips_cover_levels_workers_and_block_modes() {
    let oracle = Oracle::require();
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        ("text", text_data(), false),
        ("binary", binary_data(), true),
    ];

    for (kind, data, binary) in cases {
        let input = directory.path().join(format!("{kind}.input"));
        std::fs::write(&input, &data).unwrap();
        for level in [0, 1, 6, 9] {
            for workers in [1, 4] {
                let stem = format!("{kind}-{level}-{workers}");
                let ours_bgzf = directory.path().join(format!("{stem}.ours.bgz"));
                let hts_bgzf = directory.path().join(format!("{stem}.hts.bgz"));
                let decoded = directory.path().join(format!("{stem}.decoded"));

                let mut command = Command::new(ours());
                command
                    .arg("bgzip")
                    .arg(&input)
                    .arg("--output")
                    .arg(&ours_bgzf)
                    .arg("--compress-level")
                    .arg(level.to_string())
                    .arg("--threads")
                    .arg(workers.to_string());
                if binary {
                    command.arg("--binary");
                }
                assert_success(&command.output().unwrap());

                let decoded_by_hts = Command::new(oracle.program("bgzip"))
                    .args(["--decompress", "--stdout"])
                    .arg(&ours_bgzf)
                    .output()
                    .unwrap();
                assert_success(&decoded_by_hts);
                assert_eq!(decoded_by_hts.stdout, data, "{stem}: ours to HTSlib");

                let mut command = Command::new(oracle.program("bgzip"));
                command
                    .arg("--output")
                    .arg(&hts_bgzf)
                    .arg("--compress-level")
                    .arg(level.to_string())
                    .arg("--threads")
                    .arg(workers.to_string());
                if binary {
                    command.arg("--binary");
                }
                command.arg(&input);
                assert_success(&command.output().unwrap());

                let decoded_by_ours = Command::new(ours())
                    .args(["bgzip", "--decompress"])
                    .arg(&hts_bgzf)
                    .arg("--output")
                    .arg(&decoded)
                    .output()
                    .unwrap();
                assert_success(&decoded_by_ours);
                assert_eq!(
                    std::fs::read(&decoded).unwrap(),
                    data,
                    "{stem}: HTSlib to ours"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires HTSlib 1.24 oracles"]
fn gzi_indexes_and_partial_reads_interoperate_in_both_directions() {
    let oracle = Oracle::require();
    let directory = tempfile::tempdir().unwrap();
    let data = binary_data();
    let input = directory.path().join("input.bin");
    let ours_bgzf = directory.path().join("ours.bgz");
    let ours_gzi = directory.path().join("ours.bgz.gzi");
    let hts_bgzf = directory.path().join("hts.bgz");
    let hts_gzi = directory.path().join("hts.bgz.gzi");
    let decoded = directory.path().join("decoded.bin");
    std::fs::write(&input, &data).unwrap();

    let compressed = Command::new(ours())
        .args(["bgzip", "--binary"])
        .arg(&input)
        .arg("--output")
        .arg(&ours_bgzf)
        .arg("--index-output")
        .arg(&ours_gzi)
        .output()
        .unwrap();
    assert_success(&compressed);
    let partial = Command::new(oracle.program("bgzip"))
        .args([
            "--decompress",
            "--stdout",
            "--offset",
            "65000",
            "--size",
            "131000",
        ])
        .arg("--index-name")
        .arg(&ours_gzi)
        .arg(&ours_bgzf)
        .output()
        .unwrap();
    assert_success(&partial);
    assert_eq!(partial.stdout, data[65_000..196_000]);

    let compressed = Command::new(oracle.program("bgzip"))
        .arg("--binary")
        .arg("--index")
        .arg("--index-name")
        .arg(&hts_gzi)
        .arg("--output")
        .arg(&hts_bgzf)
        .arg(&input)
        .output()
        .unwrap();
    assert_success(&compressed);
    let partial = Command::new(ours())
        .args([
            "bgzip",
            "--decompress",
            "--offset",
            "65000",
            "--size",
            "131000",
        ])
        .arg("--index-input")
        .arg(&hts_gzi)
        .arg(&hts_bgzf)
        .arg("--output")
        .arg(&decoded)
        .output()
        .unwrap();
    assert_success(&partial);
    assert_eq!(std::fs::read(&decoded).unwrap(), data[65_000..196_000]);

    let ours_reindex = directory.path().join("ours-reindex.gzi");
    let reindexed = Command::new(ours())
        .args(["bgzip", "--reindex"])
        .arg(&hts_bgzf)
        .arg("--index-output")
        .arg(&ours_reindex)
        .output()
        .unwrap();
    assert_success(&reindexed);
    assert_eq!(read_gzi(&ours_reindex), read_gzi(&hts_gzi));

    let hts_reindex = directory.path().join("hts-reindex.gzi");
    let reindexed = Command::new(oracle.program("bgzip"))
        .arg("--reindex")
        .arg("--index-name")
        .arg(&hts_reindex)
        .arg(&ours_bgzf)
        .output()
        .unwrap();
    assert_success(&reindexed);
    assert_eq!(read_gzi(&hts_reindex), read_gzi(&ours_gzi));
}

#[test]
#[ignore = "requires HTSlib 1.24 oracles"]
fn both_tools_reject_crc_corruption_and_truncation() {
    let oracle = Oracle::require();
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.bin");
    let valid = directory.path().join("valid.bgz");
    std::fs::write(&input, binary_data()).unwrap();
    assert_success(
        &Command::new(oracle.program("bgzip"))
            .args(["--binary", "--output"])
            .arg(&valid)
            .arg(&input)
            .output()
            .unwrap(),
    );

    let original = std::fs::read(&valid).unwrap();
    let mut crc = original.clone();
    let first_block_size = usize::from(u16::from_le_bytes([crc[16], crc[17]])) + 1;
    crc[first_block_size - 8] ^= 0xff;
    let mut truncated = original;
    truncated.pop();

    for (name, bytes) in [("crc.bgz", crc), ("truncated.bgz", truncated)] {
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let ours_result = Command::new(ours())
            .args(["bgzip", "--test"])
            .arg(&path)
            .output()
            .unwrap();
        let hts_result = Command::new(oracle.program("bgzip"))
            .arg("--test")
            .arg(&path)
            .output()
            .unwrap();
        assert!(!ours_result.status.success(), "ours accepted {name}");
        assert!(!hts_result.status.success(), "HTSlib accepted {name}");
    }
}

fn read_gzi(path: &Path) -> GziIndex {
    GziIndex::read(File::open(path).unwrap()).unwrap()
}

fn text_data() -> Vec<u8> {
    let mut data = Vec::new();
    for position in 0..20_000 {
        use std::io::Write as _;
        writeln!(data, "chr1\t{position}\t{}\tACGTACGT", position + 1).unwrap();
    }
    data
}

fn binary_data() -> Vec<u8> {
    let mut state = 0x4d59_5df4_d0f3_3173u64;
    std::iter::repeat_with(|| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as u8
    })
    .take(400_000)
    .collect()
}
