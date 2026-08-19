# Performance

## Version 0.1 release gate

The release benchmark compares `rsomics-index` with HTSlib 1.24 on thirteen
BGZF and tabix paths. Every path uses three warmups followed by ten measured
pairs in alternating order. macOS `/usr/bin/time -lp` records wall, user, and
system time plus peak resident memory.

The measured revision is
`df8089c8db89b5a3e064bb01d60414a47780f4d1`. Its worktree was clean. The
complete raw and semantic evidence is retained under
`/Volumes/KIOXIA/Developments/tmp/rsomics-index-benchmark-20260820-df8089c`.

### Decision

The release gate passes on three strict throughput wins. TBI and CSI builds
are 1.36 times faster than HTSlib, win all ten paired trials, and use about
half the peak RSS. The target-scan query is 1.12 times faster and also wins all
ten pairs, although it uses more memory.

This is not a general claim that `rsomics-index` is faster than HTSlib.
Compression is 1.1% to 2.4% slower but uses 17.2% to 43.8% less peak RSS.
Binary decompression is within 0.6% of HTSlib and uses 39.3% less RSS. Text
decompression, indexed partial reads, sparse queries, dense queries, and
overlap-plus-unique queries are slower. Indexed partial reads are 14.2 times
slower, and overlap-plus-unique is 4.52 times slower while using 10.4 times
the peak RSS. These are explicit optimization targets, not hidden losses.

| Workload | rsomics median wall | HTSlib median wall | Speedup | rsomics median RSS | HTSlib median RSS | Result |
|---|---:|---:|---:|---:|---:|---|
| TBI build, 6M records | 2.700 s | 3.685 s | 1.36x | 3.1 MiB | 6.3 MiB | throughput and memory win |
| CSI build, 6M records | 2.710 s | 3.675 s | 1.36x | 3.1 MiB | 6.3 MiB | throughput and memory win |
| text compression, 1 worker | 14.940 s | 14.775 s | 0.99x | 4.6 MiB | 6.4 MiB | 1.1% slower, 28.7% less RSS |
| text compression, 4 workers | 14.865 s | 14.595 s | 0.98x | 8.0 MiB | 9.7 MiB | 1.8% slower, 17.2% less RSS |
| binary compression, 1 worker | 22.930 s | 22.425 s | 0.98x | 3.6 MiB | 6.5 MiB | 2.3% slower, 43.8% less RSS |
| binary compression, 4 workers | 16.440 s | 16.050 s | 0.98x | 6.1 MiB | 10.6 MiB | 2.4% slower, 42.8% less RSS |
| text decompression, 1 worker | 0.415 s | 0.295 s | 0.71x | 3.5 MiB | 5.9 MiB | 1.41x slower, 41.8% less RSS |
| binary decompression, 1 worker | 14.245 s | 14.170 s | 0.99x | 3.6 MiB | 5.9 MiB | 0.5% slower, 39.3% less RSS |
| 512 MiB indexed partial read | 1.135 s | 0.080 s | 0.07x | 4.2 MiB | 6.5 MiB | 14.2x slower, 35.8% less RSS |
| 10,000 sparse regions, 1 worker | 86.110 s | 47.260 s | 0.55x | 16.1 MiB | 16.9 MiB | 1.82x slower, 5.1% less RSS |
| dense regions, 4-worker budget | 0.460 s | 0.400 s | 0.87x | 16.4 MiB | 7.4 MiB | 15.0% slower, 2.21x RSS |
| overlap plus unique, 4-worker budget | 8.720 s | 1.930 s | 0.22x | 78.0 MiB | 7.5 MiB | 4.52x slower, 10.4x RSS |
| target scan, 4-worker budget | 3.410 s | 3.820 s | 1.12x | 19.0 MiB | 7.4 MiB | throughput win, 2.55x RSS |

### Workloads and equality

The deterministic text fixture contains 6,000,000 VCF records on 24 contigs.
Each record has a 320-byte payload. The plain stream is 2,189,973,389 bytes;
its HTSlib-generated BGZF representation is 38,143,892 bytes. The binary
fixture is a deterministic 2,147,483,648-byte AES-CTR stream and its BGZF
representation is 2,148,503,483 bytes.

Compression and decompression were accepted only after both tools reproduced
the complete plain-input hash. The partial lane reads 536,870,912 bytes from
offset 536,870,912 through the same GZI. The tabix workload builds TBI and CSI
over all six million records. Query lanes contain 10,000 single-position
regions, 24 dense regions, 192 overlapping regions with global deduplication,
and 24 BED targets.

| Semantic output | Bytes | SHA-256 |
|---|---:|---|
| complete VCF stream | 2,189,973,389 | `9620ea9137b89492cac7ed268111a92ce90890d3f8f615fa33335b7503578283` |
| complete binary stream | 2,147,483,648 | `4b18c8570bf559c6bdf4ae1108056c2b9bb4d32c9d3b35e91356d378e6df57e1` |
| indexed binary slice | 536,870,912 | `fec6299886342f5aff07f4e062d3a550068873c5451d1fa8bf100fd20252477f` |
| sparse query | 3,649,939 | `671e5af6c926cc8f2e8212883903152594638b0951581613d0dbe5d2ac6b104a` |
| dense query | 218,372,346 | `b94a48bb5ee4dc40a06ba8296f226d9c612b4a7e6510a0781952b2e1973f18f6` |
| overlap-plus-unique query | 1,049,742,370 | `2ae4677b917f93b65805d2dac99586a49c51aef8caf082bdafba2d3adf31c203` |
| target query | 218,867,095 | `c3750adf6d06d63ce03e5369a45330ac649db2bfea46f670db3909c6cddad1f1` |

The TBI files are byte-identical at 3,988 bytes with SHA-256
`54c3991b1b8fcd19b5585a7e10bc0ae5e638d5381d4791f8db2acd0ed08f98ae`.
CSI encodings differ in size and hash, but the pinned compatibility suite
parses them to equal index structures and the benchmark verifies both
cross-tool read directions against complete query output.

### Environment and commands

- Mac14,3 with Apple M2 and 8,589,934,592 bytes physical memory
- macOS 26.6.1 build 25G76, Darwin 25.6.0 arm64
- rustc 1.97.1, commit `8bab26f4f68e0e26f0bb7960be334d5b520ea452`
- `rsomics-index 0.1.0`
- HTSlib `bgzip` and `tabix` 1.24
- formal data on `/Volumes/Zane's HDD`; build copies and transient output on
  `/Volumes/KIOXIA`

Both index builders read independent copies of the same BGZF bytes from the
same KIOXIA filesystem. All other paired tools read the same input path.
Compression uses one or four workers for both tools. HTSlib tabix defines
`--threads` as additional threads, so query lanes use three additional HTSlib
threads versus four total rsomics workers.

```console
env CARGO_HOME=/Volumes/KIOXIA/Developments/cargo-home \
  CARGO_TARGET_DIR=/Volumes/KIOXIA/Developments/cargo-target/rsomics-index \
  TMPDIR=/Volumes/KIOXIA/Developments/tmp \
  PATH=/opt/homebrew/Cellar/rust/1.97.1/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin \
  benchmarks/index-vs-htslib.sh run \
  --records 6000000 \
  --binary-bytes 2147483648 \
  --fixture-root "/Volumes/Zane's HDD/rsomics-fixtures/index-0.1.0" \
  --result-dir /Volumes/KIOXIA/Developments/tmp/rsomics-index-benchmark-20260820-df8089c \
  --warmups 3 --runs 10
```

### Measured distributions

RSS values are bytes. The raw ledger contains all 260 timed commands,
including paired order, user time, and system time.

| Workload | Tool | Median wall | p99 wall | Mean wall | Wall stdev | Median RSS | p99 RSS |
|---|---|---:|---:|---:|---:|---:|---:|
| text compression, 1 worker | rsomics | 14.940 | 15.710 | 15.060 | 0.350619 | 4,808,704 | 5,095,424 |
| text compression, 1 worker | HTSlib | 14.775 | 15.000 | 14.784 | 0.116543 | 6,742,016 | 6,750,208 |
| text compression, 4 workers | rsomics | 14.865 | 15.090 | 14.897 | 0.102204 | 8,388,608 | 8,749,056 |
| text compression, 4 workers | HTSlib | 14.595 | 14.840 | 14.620 | 0.079162 | 10,125,312 | 10,452,992 |
| binary compression, 1 worker | rsomics | 22.930 | 23.750 | 22.944 | 0.473690 | 3,817,472 | 3,981,312 |
| binary compression, 1 worker | HTSlib | 22.425 | 22.650 | 22.358 | 0.264188 | 6,791,168 | 6,799,360 |
| binary compression, 4 workers | rsomics | 16.440 | 17.260 | 16.569 | 0.432755 | 6,356,992 | 7,143,424 |
| binary compression, 4 workers | HTSlib | 16.050 | 16.560 | 16.056 | 0.249096 | 11,116,544 | 12,025,856 |
| text decompression | rsomics | 0.415 | 0.500 | 0.428 | 0.029740 | 3,620,864 | 3,620,864 |
| text decompression | HTSlib | 0.295 | 0.320 | 0.299 | 0.013703 | 6,225,920 | 6,242,304 |
| binary decompression | rsomics | 14.245 | 14.430 | 14.238 | 0.089914 | 3,784,704 | 3,784,704 |
| binary decompression | HTSlib | 14.170 | 14.370 | 14.174 | 0.111375 | 6,234,112 | 6,242,304 |
| 512 MiB partial read | rsomics | 1.135 | 1.290 | 1.149 | 0.051521 | 4,374,528 | 4,390,912 |
| 512 MiB partial read | HTSlib | 0.080 | 0.080 | 0.080 | 0.000000 | 6,815,744 | 6,832,128 |
| TBI build | rsomics | 2.700 | 2.780 | 2.708 | 0.028597 | 3,227,648 | 3,276,800 |
| TBI build | HTSlib | 3.685 | 3.830 | 3.705 | 0.049046 | 6,602,752 | 6,651,904 |
| CSI build | rsomics | 2.710 | 2.800 | 2.714 | 0.030984 | 3,252,224 | 3,309,568 |
| CSI build | HTSlib | 3.675 | 3.840 | 3.690 | 0.056372 | 6,619,136 | 6,668,288 |
| sparse query | rsomics | 86.110 | 91.270 | 86.806 | 1.755361 | 16,850,944 | 18,808,832 |
| sparse query | HTSlib | 47.260 | 47.690 | 47.313 | 0.200003 | 17,752,064 | 17,842,176 |
| dense query | rsomics | 0.460 | 0.470 | 0.461 | 0.003162 | 17,170,432 | 17,432,576 |
| dense query | HTSlib | 0.400 | 0.480 | 0.408 | 0.025298 | 7,782,400 | 7,798,784 |
| overlap plus unique | rsomics | 8.720 | 9.000 | 8.742 | 0.100642 | 81,797,120 | 90,472,448 |
| overlap plus unique | HTSlib | 1.930 | 1.950 | 1.934 | 0.010750 | 7,880,704 | 7,929,856 |
| target scan | rsomics | 3.410 | 3.460 | 3.409 | 0.031429 | 19,914,752 | 24,068,096 |
| target scan | HTSlib | 3.820 | 3.850 | 3.819 | 0.018529 | 7,806,976 | 7,847,936 |

### Fingerprints

The benchmark ran with harness SHA-256
`2c95a7c4ce1963b310b9a9ab20e8eba9bef52c88ad67dffb644b0d7888b1254f`.
After measurement, the summary calculation was changed only to clamp a
negative floating-point roundoff variance to zero; raw measurements and
commands are unchanged. The corrected harness is
`5eb23e1b6fb60d01173a6a67caa5f753b8aee1f7d02bedb90ce171f55662a623`.
The subsequent release review narrowed the public API and moved unchanged
BGZF workflow policy between modules; the measured BGZF, tabix build, and
tabix query algorithms did not change.

| Artifact | SHA-256 |
|---|---|
| rsomics binary | `be4fffd13e36eae18b54b2b92661253733198e6fb2cbd111e20890841d0d6606` |
| HTSlib bgzip | `791a533f7bc43c604fe81f0c5044d73b4de738d78d7f0aabbcb31e7330083dd4` |
| HTSlib tabix | `65ea48411c140c34153610c2b5f6a1acb5a605eed73cd9a5dff4345efa37c9b5` |
| generation record | `abd938245b3ad0b950b7d78601e63b21fb7fd6ef6edb2cda291aec36a44ac43b` |
| fixture manifest | `86016c52970613c84df4e6ce78847a7296e75301842a2d43c73826e98ec899a8` |
| raw distribution | `83da96c313124599f99aa69911a2c85c8f6228f181afb1236a9e86c99df0193a` |
| summary | `bc7017f6fbb5e8ab5f3848d5e267c1085a6c587b439d10be3aa4cc092edb7821` |
| paired ledger | `766fb871c5af9347c6ae6ba26e7d43ad0058eac8723f8d708325ea06e3675a28` |
| equality ledger | `93051968a990646bb9dcc1e93cf67692a3cc3e218104bb1d00c1c3ed37830323` |
| provenance | `64bda06a4bf16579ca244ed88dfa5ab7c43c5f8fda16c7ac2dbfe686dfa1533b` |
| result manifest | `9ff17253fb16b8c34492939001f9c6f2d9cc1a4d0155a30beb6ad3625f44a4cb` |
