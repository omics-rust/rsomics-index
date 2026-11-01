# Third-party licenses and attribution

`rsomics-index` uses the following direct third-party Rust dependencies:

| Component | Purpose | License |
|---|---|---|
| clap | Command-line parser | MIT OR Apache-2.0 |
| crc32fast | BGZF CRC-32 generation | MIT OR Apache-2.0 |
| crossbeam-channel | Bounded worker coordination | MIT OR Apache-2.0 |
| flate2 | Sequential gzip dictionary input | MIT OR Apache-2.0 |
| zlib-rs | flate2 gzip backend | Zlib |
| libdeflater and libdeflate-sys | Native codec bindings | Apache-2.0 |
| bundled libdeflate | Deflate and gzip codec | MIT |
| md-5 | SAM sequence MD5 | MIT OR Apache-2.0 |
| noodles and noodles-bgzf | TBI, CSI, and BGZF format models | MIT |
| serde | Command-report serialization | MIT OR Apache-2.0 |

`rsomics-common` and `rsomics-help` are rsomics Layer-A dependencies, each licensed MIT OR
Apache-2.0.

HTSlib 1.24 documentation, executable behavior, and the public `bgzip`, `tabix`, TBI, and CSI
implementations are compatibility sources. The product does not link HTSlib or ship its CRAM
implementation. The applicable HTSlib MIT/Expat notice is reproduced in
[`LICENSES/HTSLIB-MIT.txt`](LICENSES/HTSLIB-MIT.txt).

samtools 1.24 `faidx`/`dict`, HTSlib FAI/GZI and SAMv1 are reference-workflow compatibility sources.
The samtools MIT/Expat notice is reproduced in [`LICENSES/SAMTOOLS-MIT.txt`](LICENSES/SAMTOOLS-MIT.txt).
The native libdeflate implementation remains an FFI-backed dependency; its bundled MIT notice is
retained in [`LICENSES/LIBDEFLATE-MIT.txt`](LICENSES/LIBDEFLATE-MIT.txt). The Rust binding packages'
Apache-2.0 license does not replace the native library's MIT terms. No HTSlib/samtools executable or
upstream runtime library is shipped in the reference fixtures.

Cargo resolves exact dependency versions in `Cargo.lock`. Transitive license obligations remain
those declared by their respective packages.
