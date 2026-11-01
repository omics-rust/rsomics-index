use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/reference")
        .join(name)
}

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rsomics-index"))
}

fn succeeded(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn captured_samtools_reference_workflow_preserves_outputs_and_named_files() {
    let evidence = std::env::var_os("RSOMICS_REFERENCE_EVIDENCE_DIR");
    let temporary = evidence.as_ref().map_or_else(
        || tempfile::tempdir().unwrap(),
        |path| {
            tempfile::Builder::new()
                .prefix("reference-")
                .tempdir_in(path)
                .unwrap()
        },
    );
    let regions = fixture("inputs/regions.txt");
    for (name, filename) in [
        ("plain", "workflow.fa"),
        ("bgzf", "workflow.fa.gz"),
        ("empty_members", "workflow.empty-members.fa.gz"),
    ] {
        let directory = temporary.path().join(name);
        fs::create_dir(&directory).unwrap();
        let reference = directory.join(filename);
        fs::copy(fixture(&format!("inputs/{filename}")), &reference).unwrap();
        let fai = directory.join("reference.fai");
        let gzi = directory.join("reference.gzi");
        let mut build = command();
        build
            .args(["faidx", "build"])
            .arg(&reference)
            .arg("--fai-idx")
            .arg(&fai);
        if name != "plain" {
            build.arg("--gzi-idx").arg(&gzi);
        }
        succeeded(build.output().unwrap());
        assert_eq!(
            fs::read(&fai).unwrap(),
            fs::read(fixture(&format!("inputs/{filename}.fai"))).unwrap()
        );
        if name != "plain" {
            assert_eq!(
                fs::read(&gzi).unwrap(),
                fs::read(fixture(&format!("inputs/{filename}.gzi"))).unwrap()
            );
        }
        for workers in [1, 4] {
            for (mode, length) in [("default", None), ("unwrapped", Some("0"))] {
                let output = directory.join(format!("{mode}-{workers}.fa"));
                let mut query = command();
                query
                    .args(["faidx", "query"])
                    .arg(&reference)
                    .arg("--fai-idx")
                    .arg(&fai)
                    .arg("-r")
                    .arg(&regions)
                    .arg("--threads")
                    .arg(workers.to_string())
                    .arg("--output")
                    .arg(&output);
                if name != "plain" {
                    query.arg("--gzi-idx").arg(&gzi);
                }
                if let Some(length) = length {
                    query.args(["--length", length]);
                }
                succeeded(query.args(["long:1-350", "crlf"]).output().unwrap());
                assert_eq!(
                    fs::read(output).unwrap(),
                    fs::read(fixture(&format!("outputs/{name}/{mode}.fa"))).unwrap()
                );
            }
        }
        let mut cross = command();
        cross
            .args(["faidx", "query"])
            .arg(&reference)
            .arg("--fai-idx")
            .arg(fixture("outputs/plain-explicit/reference.fai"));
        if name != "plain" {
            cross
                .arg("--gzi-idx")
                .arg(fixture(&format!("outputs/cross-index/{name}.bgzip.gzi")));
        }
        let result = cross
            .arg("-r")
            .arg(&regions)
            .args(["long:1-350", "crlf"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            result.stdout,
            fs::read(fixture(&format!("outputs/{name}/default.fa"))).unwrap()
        );
        let dictionary = directory.join("reference.dict");
        succeeded(
            command()
                .arg("dict")
                .arg(&reference)
                .args(["-u", "file:///reference.fa", "-o"])
                .arg(&dictionary)
                .output()
                .unwrap(),
        );
        assert_eq!(
            fs::read(dictionary).unwrap(),
            fs::read(fixture(&format!("outputs/{name}/reference.dict"))).unwrap()
        );

        let protected = directory.join("protected.fa");
        fs::write(&protected, b"keep\n").unwrap();
        for region in [
            "not_present",
            "colon:2-7",
            "long:0-1",
            "long:5-2",
            "long:18446744073709551616",
        ] {
            let mut invalid = command();
            invalid
                .args(["faidx", "query"])
                .arg(&reference)
                .arg("--fai-idx")
                .arg(&fai);
            if name != "plain" {
                invalid.arg("--gzi-idx").arg(&gzi);
            }
            let result = invalid
                .arg("--output")
                .arg(&protected)
                .arg("--force")
                .arg(region)
                .output()
                .unwrap();
            assert!(!result.status.success(), "accepted {region}");
            assert_eq!(fs::read(&protected).unwrap(), b"keep\n");
        }
        let mut json = command();
        json.args(["--json", "faidx", "query"])
            .arg(&reference)
            .arg("--fai-idx")
            .arg(&fai);
        if name != "plain" {
            json.arg("--gzi-idx").arg(&gzi);
        }
        assert!(!json.arg("long").output().unwrap().status.success());
    }
    let reference = fixture("inputs/workflow.fa");
    for (name, flags) in [
        ("alias", vec!["-A"]),
        ("no-header", vec!["-H"]),
        ("assembly", vec!["-a", "fixture-assembly"]),
        ("species", vec!["-s", "fixture species"]),
        (
            "combined",
            vec![
                "-A",
                "-H",
                "-a",
                "fixture-assembly",
                "-s",
                "fixture species",
            ],
        ),
    ] {
        let result = command()
            .arg("dict")
            .arg(&reference)
            .args(["-u", "file:///reference.fa"])
            .args(flags)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            result.stdout,
            fs::read(fixture(&format!("calls/dict-{name}/stdout.txt"))).unwrap()
        );
    }
    let result = command()
        .arg("dict")
        .arg(fixture("inputs/workflow.ordinary.fa.gz"))
        .args(["-u", "file:///reference.fa"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        result.stdout,
        fs::read(fixture("calls/dict-ordinary-gzip/stdout.txt")).unwrap()
    );
    for (input, split_gzip_magic) in [
        (reference.clone(), false),
        (fixture("inputs/workflow.ordinary.fa.gz"), true),
    ] {
        let mut stdin = command()
            .args(["dict", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let data = fs::read(input).unwrap();
        let mut pipe = stdin.stdin.take().unwrap();
        if split_gzip_magic {
            pipe.write_all(&data[..1]).unwrap();
            pipe.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
            pipe.write_all(&data[1..]).unwrap();
        } else {
            pipe.write_all(&data).unwrap();
        }
        drop(pipe);
        let result = stdin.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            result.stdout,
            fs::read(fixture("calls/dict-stdin/stdout.txt")).unwrap()
        );
    }
    assert!(
        !command()
            .args(["dict", "-"])
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .status
            .success()
    );
    for donor in ["fasta-donor", "dict-donor"] {
        let reference = temporary.path().join(format!("{donor}.fa"));
        fs::copy(fixture(&format!("inputs/{donor}.fa")), &reference).unwrap();
        succeeded(
            command()
                .args(["faidx", "build"])
                .arg(&reference)
                .output()
                .unwrap(),
        );
        let result = command()
            .args(["faidx", "query"])
            .arg(&reference)
            .args(["chr1", "chr2"])
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            result.stdout,
            fs::read(fixture(&format!("calls/{donor}-fetch/stdout.txt"))).unwrap()
        );
        let result = command()
            .arg("dict")
            .arg(&reference)
            .args(["-u", "file:///ref.fa"])
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            result.stdout,
            fs::read(fixture(&format!("calls/{donor}-dict/stdout.txt"))).unwrap()
        );
    }
    for (name, compressed) in [
        ("duplicate.fa", false),
        ("ragged.fa", false),
        ("crc-damaged.fa.gz", true),
        ("no-final-marker.fa.gz", true),
    ] {
        let fai = temporary.path().join("protected.fai");
        let gzi = temporary.path().join("protected.gzi");
        fs::write(&fai, b"original-fai\n").unwrap();
        fs::write(&gzi, b"original-gzi\n").unwrap();
        let mut build = command();
        build
            .args(["faidx", "build"])
            .arg(fixture(&format!("observations/{name}")))
            .arg("--fai-idx")
            .arg(&fai)
            .arg("--force");
        if compressed {
            build.arg("--gzi-idx").arg(&gzi);
        }
        assert!(!build.output().unwrap().status.success(), "accepted {name}");
        assert_eq!(fs::read(fai).unwrap(), b"original-fai\n");
        assert_eq!(fs::read(gzi).unwrap(), b"original-gzi\n");
    }
    assert!(
        !command()
            .args(["bgzip", "--test"])
            .arg(fixture("inputs/workflow.empty-members.fa.gz"))
            .output()
            .unwrap()
            .status
            .success()
    );
    if evidence.is_some() {
        println!("native reference evidence: {}", temporary.keep().display());
    }
}
