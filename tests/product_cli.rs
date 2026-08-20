use std::process::{Command, Output};

#[test]
fn help_exposes_only_stable_operations() {
    let output = Command::new(binary()).arg("--help").output().unwrap();

    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for present in ["bgzip", "tabix"] {
        assert!(help.contains(present), "{help}");
    }
    for absent in ["fasta-index", "dict", "fm-search"] {
        assert!(!help.contains(absent), "{help}");
    }
}

#[test]
fn json_requires_named_data_output() {
    let output = Command::new(binary())
        .args(["--json", "bgzip"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("\"status\":\"error\""), "{error}");
    assert!(error.contains("named --output"), "{error}");
}

#[test]
fn invalid_arguments_keep_clap_status_two() {
    let output = Command::new(binary())
        .args(["tabix", "unknown"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

#[test]
fn binary_completes_the_bgzf_and_tabix_workflow() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("records.vcf");
    let data = directory.path().join("records.vcf.gz");
    let gzi = directory.path().join("records.vcf.gz.gzi");
    let decoded = directory.path().join("decoded.vcf");
    let query_output = directory.path().join("query.vcf");
    std::fs::write(&input, include_bytes!("golden/records.vcf")).unwrap();

    let compressed = Command::new(binary())
        .arg("--json")
        .arg("bgzip")
        .arg(&input)
        .arg("--output")
        .arg(&data)
        .arg("--index-output")
        .arg(&gzi)
        .output()
        .unwrap();
    assert_success(&compressed);
    assert!(compressed.stderr.is_empty());
    assert!(String::from_utf8_lossy(&compressed.stdout).contains("\"command\":\"bgzip\""));
    assert!(data.is_file());
    assert!(gzi.is_file());

    let decompressed = Command::new(binary())
        .arg("bgzip")
        .arg("--decompress")
        .arg(&data)
        .arg("--output")
        .arg(&decoded)
        .output()
        .unwrap();
    assert_success(&decompressed);
    assert_eq!(
        std::fs::read(decoded).unwrap(),
        std::fs::read(&input).unwrap()
    );

    let built = Command::new(binary())
        .args(["tabix", "build"])
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&built);
    assert!(directory.path().join("records.vcf.gz.tbi").is_file());

    let queried = Command::new(binary())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("chr1:1-10")
        .output()
        .unwrap();
    assert_success(&queried);
    let queried = String::from_utf8(queried.stdout).unwrap();
    assert!(queried.contains("chr1\t7\t"), "{queried}");
    assert!(!queried.contains("chr1\t20\t"), "{queried}");

    let listed = Command::new(binary())
        .args(["tabix", "list"])
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&listed);
    assert_eq!(listed.stdout, b"chr1\nchr2\n");

    let json_query = Command::new(binary())
        .arg("--json")
        .args(["tabix", "query"])
        .arg(&data)
        .arg("chr2:1-10")
        .arg("--output")
        .arg(&query_output)
        .output()
        .unwrap();
    assert_success(&json_query);
    assert!(json_query.stderr.is_empty());
    assert!(String::from_utf8_lossy(&json_query.stdout).contains("\"operation\":\"query\""));
    assert!(String::from_utf8_lossy(&std::fs::read(query_output).unwrap()).contains("chr2\t5\t"));
}

#[test]
fn partial_tabix_configuration_starts_from_gff_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("records.gff");
    let data = directory.path().join("records.gff.gz");
    std::fs::write(&input, include_bytes!("golden/records.gff")).unwrap();

    let compressed = Command::new(binary())
        .arg("bgzip")
        .arg(&input)
        .arg("--output")
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&compressed);

    let built = Command::new(binary())
        .args(["tabix", "build", "--sequence-column", "1"])
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&built);

    let queried = Command::new(binary())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("chr1:1-12")
        .output()
        .unwrap();
    assert_success(&queried);
    assert_eq!(
        queried.stdout,
        b"chr1\tsource\tgene\t1\t10\t.\t+\t.\tID=alpha\nchr1\tsource\texon\t11\t25\t.\t+\t.\tID=beta\n"
    );
}

#[test]
fn command_adapters_reject_aliases_and_incompatible_build_options() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("records.vcf");
    let data = directory.path().join("records.vcf.gz");
    std::fs::write(&input, include_bytes!("golden/records.vcf")).unwrap();
    let compressed = Command::new(binary())
        .arg("bgzip")
        .arg(&input)
        .arg("--output")
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&compressed);
    let original = std::fs::read(&data).unwrap();

    let incompatible_index = directory.path().join("incompatible.tbi");
    let incompatible = Command::new(binary())
        .args([
            "tabix",
            "build",
            "--preset",
            "vcf",
            "--sequence-column",
            "1",
        ])
        .arg("--output")
        .arg(&incompatible_index)
        .arg(&data)
        .output()
        .unwrap();
    assert!(!incompatible.status.success());
    assert!(!incompatible_index.exists());
    assert!(String::from_utf8_lossy(&incompatible.stderr).contains("cannot be combined"));

    let built = Command::new(binary())
        .args(["tabix", "build", "--preset", "vcf"])
        .arg(&data)
        .output()
        .unwrap();
    assert_success(&built);
    let alias = Command::new(binary())
        .args(["tabix", "query"])
        .arg(&data)
        .arg("chr1")
        .arg("--output")
        .arg(&data)
        .arg("--force")
        .output()
        .unwrap();
    assert!(!alias.status.success());
    assert!(String::from_utf8_lossy(&alias.stderr).contains("also an input path"));
    assert_eq!(std::fs::read(data).unwrap(), original);

    let json_list = Command::new(binary())
        .arg("--json")
        .args(["tabix", "list"])
        .arg(directory.path().join("records.vcf.gz"))
        .output()
        .unwrap();
    assert!(!json_list.status.success());
    assert!(json_list.stdout.is_empty());
    assert!(String::from_utf8_lossy(&json_list.stderr).contains("named --output"));
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_rsomics-index")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
