# Reference workflow merge record

The target is one `rsomics-index` reference preparation/access workflow: `faidx build`, `faidx query`,
and `dict`. No public crate or foundation API was added. Common output transactions, errors, JSON,
threads and the existing help layer remain the shared interfaces. BGZF member decoding reuses the
existing BC/CRC path; a private reference reader permits empty/embedded-EOF members without
changing the old bgzip/tabix policies. Public GZI parsing retains its original strict adjacency
rule; a private reference parser accepts legitimate larger compressed gaps.

| Historical team source | Selected use | Replaced or discarded |
|---|---|---|
| `rsomics-fasta-index@fb86ed124f069141b2934641f1291a8adc83a4be`, `src/{fai,fetch}.rs` | FAI field formatting, checked stride formula and fetch intent, refactored into private typed modules | Whole-file/lossy parsing, per-request reopen, saturating coordinates, duplicated dict and old CLI/bench claim |
| `rsomics-bam-dict@e3dd9a007965f9dd741882690d973b9e51044bf3`, `src/lib.rs` | Bulk-buffered uppercase/MD5 update pattern, metadata/tag ordering and fixture option matrix | Whitespace-only filter, ordinal aliases, direct destination writes and permissive names/pre-header data |

Both donor fixtures are retained byte-exact. All scientific expected bytes come from actual
source capture, not donor claims or a Python reimplementation. No donor repository or registry
name was revived. The unrelated bam-dict lock remains untouched.

## Source authority

- samtools 1.24 commit `dc71c7274044d1050ccb64901731373ec7e915b6`: `faidx.c`, `dict.c`, versioned manuals.
- HTSlib 1.24 commit `4b705e4fada8ee2b6b15746f725ee8ac51631803`: FAI/BGZF/GZI/region source and format contracts.
- SAMv1 commit `c101c797106d8a1006b9b06f75e8bcc6062cf81e`: names, MD5 and BGZF.
- Actual hosted source run `41255333228/1`, controller `381848e07c8dceb5d30f208381e33ba17f4e9a7d`,
  samtools/HTSlib1.24 and libdeflate1.25. ZIP SHA
  `3ee721ddf17f20eb97402b86dd9daa7fb1293cbbc3ce3caa3e01c9003d857f37`.

`tests/fixtures/reference/manifest.json` binds 54 selected regular authored inputs/expected outputs
(387,576 bytes). Every selected body matched the already verified original manifest/CAS before
materialization. No symbolic link, restored executable, provider reference or clinical data was
copied or executed. Actual source observations include single-line last/no-LF FAI83/84 and GZI
compressed gaps above64KiB without repeated logical offsets.

## Intentional differences and remaining gates

Native queries never create indexes. Duplicate/empty names, empty sequences, zero coordinates and
malformed indexable layouts reject; named outputs use force/alias checks and recoverable staging.
FAI construction requires a final empty BGZF member, whereas the source missing-marker diagnostic
returned success with a warning. The source duplicate diagnostic also returned success, retaining
the first record. These deliberate strict-input/lifecycle differences follow the adopted design;
valid source output bytes are preserved. Stdout cannot be rolled back after a later error.

The new slice has captured-source tests, not a new real-reference performance acceptance or
four-native release result. No old benchmark evidence is replaced. FASTQ/strand/remote/output-BGZF
workflows remain excluded. Existing public foundation APIs and stable BGZF/tabix policies remain
unchanged.

## BGZF/query hot-path revision

The baseline `030be20895cb7806f78b2923e1c1ac3afa05cd96` has a complete correct 13-lane
BGZF/tabix campaign (`41294913111/1`) but fails the serial binary-compression and sparse-query
performance gates. This revision changes those internal paths without inheriting a speed claim.
The serial writer directly encodes into a reusable final-frame workspace with reusable input,
while parallel work remains ordered and bounded. Neither output transactions nor sync policy change.

HTSlib 1.24 `bgzf.c:586` maps public levels 0..9 to libdeflate efforts
`0,1,2,3,5,6,7,8,10,12`; the former implementation passed nonzero public levels through unchanged.
Both serial and parallel writers now use the source mapping. Level 0 keeps explicit stored framing.
This effort/quality correction changes compressed bytes and invalidates transfer of older timing
or output-size claims; decoded-content and framing contracts remain unchanged.

The indexed reader uses buffered physical seeks and reusable frame/uncached decode storage. The
configured bounded cache is honored for forward requests and uses FIFO retention rather than
per-hit queue scans. Query fragments now filter on their own bounds, fixing duplicate output when
a unique request has multiple uncovered fragments. Long BED/VCF overlaps, ordered requests,
targets and reference identity remain part of selection. Pinned noodles-csi0.56.0 already returns
sorted optimized chunks, so the redundant consumer sort/merge is removed.

Early stop ends suffix record parsing/visitation only. Full existing index-chunk framing, CRC,
uncompressed offsets and EOF validation still run, including queued parallel results; no claim is
made that all suffix I/O or decode work stops. Whole native-four and same-input fixed 13-lane
qualification remain pending for this changed source. No new public item or foundation is added.
