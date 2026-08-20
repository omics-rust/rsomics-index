# rsomics-index

`rsomics-index` prepares and queries local BGZF-compressed genomic resources. Version 0.1 contains
one complete BGZF workflow and one complete tabix workflow:

```text
rsomics-index bgzip
rsomics-index tabix build
rsomics-index tabix query
rsomics-index tabix list
```

FASTA indexes, sequence dictionaries, remote URIs, and exact substring indexes are not exposed in
this release.

## Install

```bash
cargo install rsomics-index
rsomics-index --help
```

The command tree and all nested help use the shared `rsomics-help` interaction layer. Runtime
errors and `--json` reports use the shared `rsomics-common` output contract.

## BGZF

Compress a file without deleting the input:

```bash
rsomics-index bgzip calls.vcf \
  --output calls.vcf.gz \
  --index-output calls.vcf.gz.gzi \
  --threads 4
```

Compression levels 0 through 9 are supported. Text mode is the default and keeps full block
boundaries on newlines when possible; `--binary` fills blocks without that preference. Every
stream ends with exactly one canonical BGZF EOF member.

Decompress or validate a stream:

```bash
rsomics-index bgzip --decompress calls.vcf.gz --output calls.vcf
rsomics-index bgzip --test calls.vcf.gz
```

Use a GZI sidecar for a zero-based uncompressed byte range:

```bash
rsomics-index bgzip --decompress calls.vcf.gz \
  --index-input calls.vcf.gz.gzi \
  --offset 1000000 \
  --size 65536 \
  --output slice.bin
```

Rebuild a sidecar with `--reindex`. Named outputs are staged beside their destinations and become
visible only after successful finalization. Existing outputs require `--force`; input files are
never removed.

## Tabix

Build TBI for a sorted VCF:

```bash
rsomics-index tabix build --preset vcf calls.vcf.gz
```

The supported presets are `bed`, `gff`, `sam`, and `vcf`. If neither a preset nor custom columns
are supplied, format detection uses the file name and a bounded decompressed sample. Custom
columns are one-based column numbers:

```bash
rsomics-index tabix build table.tsv.gz \
  --sequence-column 1 \
  --begin-column 2 \
  --end-column 3 \
  --zero-based
```

TBI accepts coordinates through base 536,870,912. Use CSI for larger coordinate spaces or when a
different minimum shift is required:

```bash
rsomics-index tabix build --preset vcf --csi --min-shift 14 calls.vcf.gz
```

Inline regions are one-based inclusive. A file ending in `.bed` is interpreted as zero-based
half-open; other tabular region and target files are one-based inclusive.

```bash
rsomics-index tabix query calls.vcf.gz chr2:1000-2000
rsomics-index tabix query calls.vcf.gz \
  --regions-file regions.tsv \
  --targets-file targets.bed \
  --print-header \
  --output selected.vcf
rsomics-index tabix list calls.vcf.gz
```

Region queries preserve request order. Target-only queries scan once and preserve data-file order.
`--unique` deduplicates physical records across regions; `--separate-regions` emits a marker before
each region and is mutually exclusive with `--unique`. `--threads` controls BGZF decoding workers,
and `--cache-bytes 0` disables the bounded decompressed-block cache.

## Machine-readable reports

`--json` reserves standard output for one result envelope. Any command that normally writes data
or reference names to standard output therefore requires a named `--output` under `--json`:

```bash
rsomics-index --json tabix query calls.vcf.gz chr1 \
  --output chr1.vcf
```

The data goes to `chr1.vcf`; the command summary goes to standard output. Structured failures go
to standard error and retain a nonzero exit status.

## Compatibility and scope

The behavior target for BGZF and tabix is HTSlib 1.24. Compatibility means decoded content,
coordinate selection, index semantics, and observable failures; compressed bytes need not match
because valid deflate streams and block boundaries can differ.

Version 0.1 supports local files and standard streams only. It deliberately excludes remote index
discovery, authentication, in-place input replacement, multi-input invocations, rebgzip layout
reproduction, and metadata copying.

## Performance

The exact-head 6,000,000-record release benchmark records strict wins for TBI and CSI construction,
binary compression, and all four query workloads, with lower median peak RSS in every measured
path. Text compression, full decompression, and indexed partial reads remain slower and are not
presented as wins. Complete distributions, losses, hashes, and machine provenance are in
[`PERFORMANCE.md`](PERFORMANCE.md).

The formal harness builds the release binary itself and binds its SHA-256 to the clean Git head
before correctness or timing work can start:

```console
env CARGO_HOME=/Volumes/KIOXIA/Developments/cargo-home \
  CARGO_TARGET_DIR=/Volumes/KIOXIA/Developments/cargo-target/rsomics-index \
  TMPDIR=/Volumes/KIOXIA/Developments/tmp \
  benchmarks/index-vs-htslib.sh build

env CARGO_HOME=/Volumes/KIOXIA/Developments/cargo-home \
  CARGO_TARGET_DIR=/Volumes/KIOXIA/Developments/cargo-target/rsomics-index \
  TMPDIR=/Volumes/KIOXIA/Developments/tmp \
  benchmarks/index-vs-htslib.sh run \
  --result-dir /Volumes/KIOXIA/Developments/tmp/rsomics-index-benchmark-current
```

## License

`rsomics-index` is available under MIT OR Apache-2.0. See
[`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md) for dependency and compatibility-source
attribution.
