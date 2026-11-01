# rsomics-index

`rsomics-index` prepares and queries local genomic resources. The current tree contains
BGZF, tabix, FASTA random access, and sequence-dictionary workflows:

```text
rsomics-index bgzip
rsomics-index tabix build
rsomics-index tabix query
rsomics-index tabix list
rsomics-index faidx build
rsomics-index faidx query
rsomics-index dict
```

Publication remains held. The new reference slice has captured samtools 1.24 output comparisons;
its real-reference performance and four-native release qualification are still pending.
Remote URIs and exact substring indexes are not exposed.

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

## FASTA references and dictionaries

Build FAI for plain FASTA, or FAI and GZI together for BGZF:

```bash
rsomics-index faidx build reference.fa
rsomics-index faidx build reference.fa.gz
rsomics-index faidx query reference.fa.gz chr1:1000-2000 chr2 --threads 4
rsomics-index faidx query reference.fa.gz --region-file regions.txt --length 0 --output selected.fa
rsomics-index dict reference.fa.gz --alias --assembly GRCh37 --output reference.dict
```

`--fai-idx` and `--gzi-idx` select explicit sidecar paths. Query requires existing indexes and never
creates or rebuilds them implicitly. An FAI must match the unchanged reference bytes; it is not a
content checksum. Plain and BGZF FASTA are indexable; ordinary gzip is sequential dict input only.

Regions are one-based inclusive. Whole names, `name:start-end`, `name:start`, `name:start-`, and
`name:-end` are supported; a single coordinate means through the end, not one base. Use braces for
ambiguous colon-bearing names: `'{chr1}:100-200'` or `'{chr1:100-200}'`. Region-file requests precede
positional requests, preserving duplicates and literal output headers. Positive out-of-range
coordinates are clipped with a warning. Zero, malformed, reversed, and overflowing coordinates fail.

Default FASTA wrapping follows each input record's line length. `--length 0` disables wrapping.
FAI construction scans once, retaining raw LF/CRLF widths; the final no-newline line follows
samtools 1.24's FAI width convention. Query uses bounded ordered 1 MiB chunks across requests and
opens one reference reader per worker. `--threads` counts total query workers and defaults to one;
build and dict do not advertise fake parallelism.

Reference BGZF reading accepts valid empty members and embedded EOF members; the build requires a
final empty member. Random queries validate touched members rather than scanning the complete
reference. Existing `bgzip` and tabix integrity policies remain unchanged. Indexable sequence lines
must have a consistent stride except the final line, with no embedded whitespace. Names are unique
nonempty printable ASCII. Empty sequences and duplicate names fail; samtools may instead retain
the first duplicate. Standard FASTQ, reverse complement, strand labeling, alternate-locus metadata,
compressed query output and output indexing are deliberately absent.

`dict` accepts plain, gzip, or BGZF FASTA and `-` for stdin. It streams SAM MD5 calculation, stripping
bytes outside ASCII 33–126 and uppercasing ASCII. Default UR is the canonical file URI, absent for
stdin; `--uri` overrides it. `--alias` adds samtools' chr/M/MT alternatives, not ordinal identifiers.
Dictionary SN names must satisfy SAM syntax. `--no-header`, `--assembly`, and `--species` follow
samtools 1.24. Named outputs require `--force` to replace existing files. FAI/GZI are staged together
as a recoverable normal-error transaction, not a crash-atomic filesystem-wide commit.

## Machine-readable reports

`--json` reserves standard output for one result envelope. Any command that normally writes data
or reference names to standard output therefore requires a named `--output` under `--json`:

```bash
rsomics-index --json tabix query calls.vcf.gz chr1 \
  --output chr1.vcf
```

The data goes to `chr1.vcf`; the command summary goes to standard output. Structured failures go
to standard error and retain a nonzero exit status.

The same rule applies to `faidx query` and `dict`: named data output under `--json`, with the shared
result envelope on stdout. No product-specific JSON wrapper is added around FASTA or SAM bytes.

## Compatibility and scope

The behavior target for BGZF and tabix is HTSlib 1.24. Compatibility means decoded content,
coordinate selection, index semantics, and observable failures; compressed bytes need not match
because valid deflate streams and block boundaries can differ.

FASTA and dictionary behavior targets samtools/HTSlib 1.24. The committed compact fixtures retain
actual hosted source inputs and output bytes with a hash manifest; native tests compare FAI/GZI,
multi-region wrapping, dictionaries and 1/4-worker outputs without executing an upstream binary.
See [`MERGE_RECORD.md`](MERGE_RECORD.md) for immutable source and donor provenance.

Version 0.1 supports local files and standard streams only. It deliberately excludes remote index
discovery, authentication, in-place input replacement, multi-input invocations, rebgzip layout
reproduction, and metadata copying.

## Performance

No FASTA/dictionary throughput or memory advantage is claimed yet. The older BGZF/tabix numbers
below are historical; their missing raw-evidence/publication hold is recorded in `PERFORMANCE.md`.
This implementation does not restore or reinterpret those trials.

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
