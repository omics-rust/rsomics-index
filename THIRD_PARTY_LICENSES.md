# Third-party licenses and attribution

`rsomics-index` uses the following direct third-party Rust dependencies:

| Component | Purpose | License |
|---|---|---|
| clap | Command-line parser | MIT OR Apache-2.0 |
| crc32fast | BGZF CRC-32 generation | MIT OR Apache-2.0 |
| crossbeam-channel | Bounded worker coordination | MIT OR Apache-2.0 |
| libdeflater and libdeflate | Deflate and gzip codec | Apache-2.0 |
| noodles and noodles-bgzf | TBI, CSI, and BGZF format models | MIT |
| serde | Command-report serialization | MIT OR Apache-2.0 |

`rsomics-common` and `rsomics-help` are rsomics Layer-A dependencies, each licensed MIT OR
Apache-2.0.

HTSlib 1.24 documentation, executable behavior, and the public `bgzip`, `tabix`, TBI, and CSI
implementations are compatibility sources. The product does not link HTSlib or ship its CRAM
implementation. The applicable HTSlib MIT/Expat notice is reproduced in
[`LICENSES/HTSLIB-MIT.txt`](LICENSES/HTSLIB-MIT.txt).

Cargo resolves exact dependency versions in `Cargo.lock`. Transitive license obligations remain
those declared by their respective packages.
