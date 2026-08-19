use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct Oracle {
    directory: PathBuf,
}

impl Oracle {
    pub fn require() -> Self {
        let directory = std::env::var_os("RSOMICS_HTSLIB_ORACLE_DIR")
            .map(PathBuf::from)
            .expect("RSOMICS_HTSLIB_ORACLE_DIR is required for ignored compatibility tests");
        let oracle = Self { directory };
        for program in ["bgzip", "tabix"] {
            let output = Command::new(oracle.program(program))
                .arg("--version")
                .output()
                .unwrap();
            assert_success(&output);
            let version = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                version
                    .lines()
                    .next()
                    .is_some_and(|line| line.ends_with("1.24")),
                "expected {program} 1.24, got {version:?}"
            );
        }
        oracle
    }

    pub fn program(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
}

pub fn ours() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_rsomics-index"))
}

pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
