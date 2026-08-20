#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

repository=$(cd "$(dirname "$0")/.." && pwd)
binary=/Volumes/KIOXIA/Developments/cargo-target/rsomics-index/release/rsomics-index
binary_overridden=false
oracle_directory=/opt/homebrew/bin
fixture_root="/Volumes/Zane's HDD/rsomics-fixtures/index-0.1.0"
result_directory=/Volumes/KIOXIA/Developments/tmp/rsomics-index-benchmark
records=6000000
binary_bytes=2147483648
threads=4
warmups=3
runs=10

usage() {
    cat >&2 <<EOF
usage: $0 build|generate|smoke|run|summarize [options]

options:
  --binary PATH
  --oracle-dir DIRECTORY
  --fixture-root DIRECTORY
  --result-dir DIRECTORY
  --records INTEGER
  --binary-bytes INTEGER
  --threads INTEGER
  --warmups INTEGER
  --runs INTEGER
EOF
    exit 2
}

die() {
    printf '%s\n' "$*" >&2
    exit 2
}

external_path() {
    case "$1" in
        /Volumes/*) ;;
        *) die "benchmark data, binaries, and results must remain on external volumes: $1" ;;
    esac
}

positive_integer() {
    [[ "$2" =~ ^[1-9][0-9]*$ ]] || die "$1 must be a positive integer"
}

resolve_executable() {
    local resolved
    if [[ "$1" == */* ]]; then
        resolved=$(realpath "$1")
    else
        resolved=$(command -v "$1")
    fi
    [[ -x "$resolved" ]] || die "executable not found: $1"
    printf '%s\n' "$resolved"
}

parse_options() {
    while (($#)); do
        case "$1" in
            --binary) binary=${2:?}; binary_overridden=true; shift 2 ;;
            --oracle-dir) oracle_directory=${2:?}; shift 2 ;;
            --fixture-root) fixture_root=${2:?}; shift 2 ;;
            --result-dir) result_directory=${2:?}; shift 2 ;;
            --records) records=${2:?}; shift 2 ;;
            --binary-bytes) binary_bytes=${2:?}; shift 2 ;;
            --threads) threads=${2:?}; shift 2 ;;
            --warmups) warmups=${2:?}; shift 2 ;;
            --runs) runs=${2:?}; shift 2 ;;
            *) usage ;;
        esac
    done
}

require_clean_head() {
    [[ -z $(git -C "$repository" status --porcelain) ]] \
        || die "the repository must be clean"
}

validate_execution_environment() {
    local expected_cargo_home=/Volumes/KIOXIA/Developments/cargo-home
    local expected_target=/Volumes/KIOXIA/Developments/cargo-target/rsomics-index
    local expected_tmp=/Volumes/KIOXIA/Developments/tmp
    local root_usage

    [[ ${CARGO_HOME:-} == "$expected_cargo_home" ]] \
        || die "CARGO_HOME must be $expected_cargo_home"
    [[ ${CARGO_TARGET_DIR:-} == "$expected_target" ]] \
        || die "CARGO_TARGET_DIR must be $expected_target"
    [[ ${TMPDIR:-} == "$expected_tmp" ]] \
        || die "TMPDIR must be $expected_tmp"
    external_path "$CARGO_HOME"
    external_path "$CARGO_TARGET_DIR"
    external_path "$TMPDIR"
    root_usage=$(df -P / | awk 'NR == 2 { gsub("%", "", $5); print $5 }')
    [[ "$root_usage" =~ ^[0-9]+$ ]] || die "could not determine boot-disk usage"
    ((root_usage < 80)) \
        || die "boot disk is ${root_usage}% full; build, test, and benchmark work is prohibited"
    df -h / /Volumes/KIOXIA >&2
}

build_binary() {
    local expected_binary manifest stage head binary_sha lock_sha
    validate_execution_environment
    require_clean_head
    [[ $binary_overridden == false ]] \
        || die "build uses the repository's fixed external target; omit --binary"

    expected_binary="$CARGO_TARGET_DIR/release/rsomics-index"
    cargo build --manifest-path "$repository/Cargo.toml" --locked --release --bin rsomics-index
    [[ -x "$expected_binary" ]] || die "build did not produce $expected_binary"

    binary=$(realpath "$expected_binary")
    manifest="$binary.build-provenance"
    head=$(git -C "$repository" rev-parse HEAD)
    binary_sha=$(shasum -a 256 "$binary" | awk '{ print $1 }')
    lock_sha=$(shasum -a 256 "$repository/Cargo.lock" | awk '{ print $1 }')
    stage=$(mktemp "$TMPDIR/rsomics-index-build-provenance.XXXXXX")
    {
        printf 'git_head=%s\n' "$head"
        printf 'git_dirty=false\n'
        printf 'binary_sha256=%s\n' "$binary_sha"
        printf 'cargo_lock_sha256=%s\n' "$lock_sha"
        printf 'binary=%s\n' "$binary"
        printf 'built_utc=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
        rustc --version --verbose
        cargo --version --verbose
    } > "$stage"
    mv "$stage" "$manifest"
    printf 'built and recorded exact head %s: %s\n' "$head" "$manifest"
}

manifest_value() {
    local key=$1
    awk -F= -v key="$key" '$1 == key { sub(/^[^=]*=/, ""); print; exit }' \
        "$binary.build-provenance"
}

verify_binary_provenance() {
    local manifest="$binary.build-provenance"
    local current_head recorded_head current_sha recorded_sha current_lock recorded_lock
    [[ -f "$manifest" ]] \
        || die "missing $manifest; run '$0 build' from the clean benchmark head"
    require_clean_head

    current_head=$(git -C "$repository" rev-parse HEAD)
    recorded_head=$(manifest_value git_head)
    [[ "$recorded_head" == "$current_head" ]] \
        || die "binary was built from $recorded_head, not current head $current_head"
    [[ $(manifest_value git_dirty) == false ]] \
        || die "binary provenance does not describe a clean build"
    [[ $(manifest_value binary) == "$binary" ]] \
        || die "binary path does not match its build provenance"

    current_sha=$(shasum -a 256 "$binary" | awk '{ print $1 }')
    recorded_sha=$(manifest_value binary_sha256)
    [[ "$recorded_sha" == "$current_sha" ]] \
        || die "binary checksum does not match its build provenance"
    current_lock=$(shasum -a 256 "$repository/Cargo.lock" | awk '{ print $1 }')
    recorded_lock=$(manifest_value cargo_lock_sha256)
    [[ "$recorded_lock" == "$current_lock" ]] \
        || die "Cargo.lock does not match the binary build"
}

prepare() {
    validate_execution_environment
    positive_integer records "$records"
    positive_integer binary_bytes "$binary_bytes"
    positive_integer threads "$threads"
    positive_integer warmups "$warmups"
    positive_integer runs "$runs"
    ((binary_bytes % 1048576 == 0)) || die "--binary-bytes must be a multiple of 1048576"

    binary=$(resolve_executable "$binary")
    oracle_directory=$(realpath "$oracle_directory")
    bgzip=$(resolve_executable "$oracle_directory/bgzip")
    tabix=$(resolve_executable "$oracle_directory/tabix")
    external_path "$binary"

    mkdir -p "$fixture_root" "$result_directory"
    fixture_root=$(realpath "$fixture_root")
    result_directory=$(realpath "$result_directory")
    external_path "$fixture_root"
    external_path "$result_directory"

    [[ $("$binary" --version) == "rsomics-index 0.1.0" ]] \
        || die "rsomics-index 0.1.0 is required"
    [[ $("$bgzip" --version | sed -n '1p') == "bgzip (htslib) 1.24" ]] \
        || die "HTSlib bgzip 1.24 is required"
    [[ $("$tabix" --version | sed -n '1p') == "tabix (htslib) 1.24" ]] \
        || die "HTSlib tabix 1.24 is required"

    if [[ $mode == smoke || $mode == run ]]; then
        verify_binary_provenance
    fi

    fixture_directory="$fixture_root/formal-${records}-${binary_bytes}"
    vcf="$fixture_directory/records.vcf"
    vcf_bgzf="$fixture_directory/records.vcf.gz"
    vcf_tbi="$vcf_bgzf.tbi"
    vcf_csi="$vcf_bgzf.csi"
    binary_input="$fixture_directory/incompressible.bin"
    binary_bgzf="$fixture_directory/incompressible.bin.bgz"
    binary_gzi="$binary_bgzf.gzi"
    sparse_regions="$fixture_directory/regions-sparse.tsv"
    dense_regions="$fixture_directory/regions-dense.tsv"
    overlap_regions="$fixture_directory/regions-overlap.tsv"
    targets="$fixture_directory/targets.bed"
    partial_offset=$((binary_bytes / 4))
    partial_size=$((binary_bytes / 4))
}

generate() {
    if [[ -f "$fixture_directory/SHA256SUMS" ]] \
        && (cd "$fixture_directory" && shasum -a 256 --check SHA256SUMS >/dev/null); then
        printf 'fixture already verified: %s\n' "$fixture_directory"
        return
    fi
    [[ ! -e "$fixture_directory" ]] \
        || die "fixture directory exists without a valid manifest: $fixture_directory"

    local stage filler records_per_contig blocks
    stage=$(mktemp -d "$fixture_root/.generate.XXXXXX")
    cleanup_stage() {
        if [[ -n "${stage:-}" && -d "$stage" ]]; then
            rm -rf "$stage"
        fi
    }
    trap cleanup_stage EXIT

    filler=$(awk 'BEGIN { for (i = 0; i < 80; i++) printf "ACGT" }')
    records_per_contig=$(((records + 23) / 24))
    awk -v records="$records" -v per="$records_per_contig" -v filler="$filler" 'BEGIN {
        print "##fileformat=VCFv4.3"
        print "##INFO=<ID=PAYLOAD,Number=1,Type=String,Description=\"Deterministic benchmark payload\">"
        print "##INFO=<ID=N,Number=1,Type=Integer,Description=\"Record ordinal\">"
        for (chromosome = 1; chromosome <= 24; chromosome++)
            print "##contig=<ID=chr" chromosome ",length=" per ">"
        print "#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO"
        for (i = 0; i < records; i++) {
            chromosome = int(i / per) + 1
            position = i % per + 1
            print "chr" chromosome "\t" position "\t.\tA\tC\t50\tPASS\tPAYLOAD=" filler ";N=" i
        }
    }' > "$stage/records.vcf"

    blocks=$((binary_bytes / 1048576))
    dd if=/dev/zero bs=1048576 count="$blocks" 2>/dev/null \
        | openssl enc -aes-256-ctr -nosalt \
            -K 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
            -iv fedcba9876543210fedcba9876543210 \
        > "$stage/incompressible.bin"

    "$bgzip" --force --threads "$threads" --compress-level 6 \
        --output "$stage/records.vcf.gz" "$stage/records.vcf"
    "$tabix" --force --preset vcf "$stage/records.vcf.gz"
    "$tabix" --force --preset vcf --csi "$stage/records.vcf.gz"
    "$bgzip" --force --binary --threads "$threads" --compress-level 6 --index \
        --index-name "$stage/incompressible.bin.bgz.gzi" \
        --output "$stage/incompressible.bin.bgz" "$stage/incompressible.bin"

    awk -v records="$records" -v per="$records_per_contig" 'BEGIN {
        count = records < 10000 ? records : 10000
        for (i = 0; i < count; i++) {
            ordinal = int(i * records / count)
            chromosome = int(ordinal / per) + 1
            position = ordinal % per + 1
            print "chr" chromosome "\t" position "\t" position
        }
    }' > "$stage/regions-sparse.tsv"
    awk -v per="$records_per_contig" 'BEGIN {
        end = per < 25000 ? per : 25000
        for (chromosome = 1; chromosome <= 24; chromosome++)
            print "chr" chromosome "\t1\t" end
    }' > "$stage/regions-dense.tsv"
    awk -v per="$records_per_contig" 'BEGIN {
        width = per < 50000 ? per : 50000
        step = int(width / 5)
        if (step < 1) step = 1
        for (chromosome = 1; chromosome <= 24; chromosome++) {
            for (region = 0; region < 8; region++) {
                start = 1 + region * step
                if (start > per) break
                end = start + width - 1
                if (end > per) end = per
                print "chr" chromosome "\t" start "\t" end
            }
        }
    }' > "$stage/regions-overlap.tsv"
    awk -v per="$records_per_contig" 'BEGIN {
        start = int(per / 3)
        end = start + 25000
        if (end > per) end = per
        for (chromosome = 1; chromosome <= 24; chromosome++)
            print "chr" chromosome "\t" start - 1 "\t" end
    }' > "$stage/targets.bed"

    {
        printf 'records=%s\n' "$records"
        printf 'binary_bytes=%s\n' "$binary_bytes"
        printf 'contigs=24\n'
        printf 'payload_bytes=320\n'
        printf 'generator=awk+openssl-aes-256-ctr\n'
        printf 'bgzip=%s\n' "$("$bgzip" --version | sed -n '1p')"
        printf 'tabix=%s\n' "$("$tabix" --version | sed -n '1p')"
    } > "$stage/GENERATION.txt"
    (
        cd "$stage"
        shasum -a 256 GENERATION.txt records.vcf records.vcf.gz records.vcf.gz.tbi \
            records.vcf.gz.csi incompressible.bin incompressible.bin.bgz \
            incompressible.bin.bgz.gzi regions-sparse.tsv regions-dense.tsv \
            regions-overlap.tsv targets.bed > SHA256SUMS
    )
    mv "$stage" "$fixture_directory"
    stage=
    trap - EXIT
    printf 'generated and verified: %s\n' "$fixture_directory"
}

require_fixture() {
    [[ -f "$fixture_directory/SHA256SUMS" ]] || die "run generate first"
    (cd "$fixture_directory" && shasum -a 256 --check SHA256SUMS >/dev/null) \
        || die "fixture checksum verification failed"
}

file_signature() {
    local path=$1
    printf '%s\t%s' "$(stat -f '%z' "$path")" "$(shasum -a 256 "$path" | awk '{print $1}')"
}

record_signature() {
    local workload=$1
    local tool=$2
    local path=$3
    local expected=${4:-}
    local signature
    signature=$(file_signature "$path")
    printf '%s\t%s\t%s\n' "$workload" "$tool" "$signature" >> "$correctness"
    if [[ -n "$expected" && "$signature" != "$expected" ]]; then
        die "$workload output mismatch for $tool"
    fi
    printf '%s' "$signature"
}

capture_stdout() {
    local workload=$1
    local tool=$2
    local expected=${3:-}
    shift 3
    local output="$scratch/correctness-output"
    rm -f "$output"
    "$@" > "$output"
    record_signature "$workload" "$tool" "$output" "$expected"
    rm -f "$output"
}

validate_cross_index() {
    local ours_index=$1
    local hts_index=$2
    local hts_data=$3
    local saved="$scratch/saved-index"
    local ours_output="$scratch/ours-index-query"
    local hts_output="$scratch/hts-index-query"
    cp "$hts_index" "$saved"

    "$binary" tabix query "$vcf_bgzf" --index "$ours_index" chr1:1-100 \
        > "$ours_output"
    cp "$ours_index" "$hts_index"
    touch "$hts_index"
    "$tabix" --verbosity 0 "$hts_data" chr1:1-100 > "$hts_output"
    cmp "$ours_output" "$hts_output"

    cp "$saved" "$hts_index"
    "$binary" tabix query "$vcf_bgzf" --index "$hts_index" chr1:1-100 \
        > "$ours_output"
    "$tabix" --verbosity 0 "$hts_data" chr1:1-100 > "$hts_output"
    cmp "$ours_output" "$hts_output"
    rm -f "$saved" "$ours_output" "$hts_output"
}

smoke() {
    if [[ $mode == smoke ]]; then
        [[ -z $(find "$result_directory" -mindepth 1 -maxdepth 1 -print -quit) ]] \
            || die "smoke result directory must be empty: $result_directory"
    fi
    require_fixture
    scratch="$result_directory/scratch"
    mkdir -p "$scratch"
    correctness="$result_directory/correctness.tsv"
    printf 'workload\ttool\toutput_bytes\tsha256\n' > "$correctness"

    local input_signature ours_signature ours_output hts_output decoded
    local hts_signature index_ours index_hts ours_build_data hts_build_data

    input_signature=$(file_signature "$vcf")
    ours_output="$scratch/ours-text.bgz"
    hts_output="$scratch/hts-text.bgz"
    rm -f "$ours_output" "$hts_output"
    "$binary" bgzip "$vcf" --output "$ours_output" --threads "$threads" \
        --compress-level 6 --force
    "$bgzip" --force --threads "$threads" --compress-level 6 \
        --output "$hts_output" "$vcf"
    decoded="$scratch/decoded"
    rm -f "$decoded"
    "$binary" bgzip --decompress "$ours_output" > "$decoded"
    record_signature compress_text rsomics "$decoded" "$input_signature" >/dev/null
    rm -f "$decoded"
    "$binary" bgzip --decompress "$hts_output" > "$decoded"
    record_signature compress_text htslib "$decoded" "$input_signature" >/dev/null
    rm -f "$decoded" "$ours_output" "$hts_output"

    input_signature=$(file_signature "$binary_input")
    ours_output="$scratch/ours-binary.bgz"
    hts_output="$scratch/hts-binary.bgz"
    rm -f "$ours_output" "$hts_output"
    "$binary" bgzip "$binary_input" --binary --output "$ours_output" \
        --threads "$threads" --compress-level 6 --force
    "$bgzip" --force --binary --threads "$threads" --compress-level 6 \
        --output "$hts_output" "$binary_input"
    "$binary" bgzip --decompress "$ours_output" > "$decoded"
    record_signature compress_binary rsomics "$decoded" "$input_signature" >/dev/null
    rm -f "$decoded"
    "$binary" bgzip --decompress "$hts_output" > "$decoded"
    record_signature compress_binary htslib "$decoded" "$input_signature" >/dev/null
    rm -f "$decoded" "$ours_output" "$hts_output"

    capture_stdout decompress_text rsomics "$(file_signature "$vcf")" \
        "$binary" bgzip --decompress "$vcf_bgzf" >/dev/null
    capture_stdout decompress_text htslib "$(file_signature "$vcf")" \
        "$bgzip" --decompress --stdout "$vcf_bgzf" >/dev/null
    capture_stdout decompress_binary rsomics "$(file_signature "$binary_input")" \
        "$binary" bgzip --decompress "$binary_bgzf" >/dev/null
    capture_stdout decompress_binary htslib "$(file_signature "$binary_input")" \
        "$bgzip" --decompress --stdout "$binary_bgzf" >/dev/null

    ours_signature=$(capture_stdout partial_binary rsomics "" \
        "$binary" bgzip --decompress "$binary_bgzf" --index-input "$binary_gzi" \
            --offset "$partial_offset" --size "$partial_size")
    capture_stdout partial_binary htslib "$ours_signature" \
        "$bgzip" --decompress --stdout --index-name "$binary_gzi" \
            --offset "$partial_offset" --size "$partial_size" "$binary_bgzf" >/dev/null

    index_ours="$scratch/ours.tbi"
    ours_build_data="$scratch/ours-build.vcf.gz"
    hts_build_data="$scratch/hts-build.vcf.gz"
    index_hts="$hts_build_data.tbi"
    rm -f "$index_ours" "$ours_build_data" "$hts_build_data" "$index_hts"
    cp "$vcf_bgzf" "$ours_build_data"
    cp "$vcf_bgzf" "$hts_build_data"
    "$binary" tabix build --preset vcf --output "$index_ours" "$ours_build_data"
    "$tabix" --force --preset vcf "$hts_build_data"
    validate_cross_index "$index_ours" "$index_hts" "$hts_build_data"
    record_signature build_tbi rsomics "$index_ours" >/dev/null
    record_signature build_tbi htslib "$index_hts" >/dev/null

    index_ours="$scratch/ours.csi"
    index_hts="$hts_build_data.csi"
    rm -f "$index_ours" "$index_hts" "$hts_build_data.tbi"
    "$binary" tabix build --preset vcf --csi --output "$index_ours" "$ours_build_data"
    "$tabix" --force --preset vcf --csi "$hts_build_data"
    validate_cross_index "$index_ours" "$index_hts" "$hts_build_data"
    record_signature build_csi rsomics "$index_ours" >/dev/null
    record_signature build_csi htslib "$index_hts" >/dev/null

    ours_signature=$(capture_stdout query_sparse rsomics "" \
        "$binary" tabix query "$vcf_bgzf" --index "$vcf_tbi" \
            --regions-file "$sparse_regions")
    capture_stdout query_sparse htslib "$ours_signature" \
        "$tabix" --regions "$sparse_regions" "$vcf_bgzf" >/dev/null
    ours_signature=$(capture_stdout query_dense rsomics "" \
        "$binary" tabix query "$vcf_bgzf" --index "$vcf_tbi" \
            --regions-file "$dense_regions" --threads "$threads")
    capture_stdout query_dense htslib "$ours_signature" \
        "$tabix" --regions "$dense_regions" --threads "$((threads - 1))" \
            "$vcf_bgzf" >/dev/null
    ours_signature=$(capture_stdout query_overlap_unique rsomics "" \
        "$binary" tabix query "$vcf_bgzf" --index "$vcf_tbi" \
            --regions-file "$overlap_regions" --unique --threads "$threads")
    capture_stdout query_overlap_unique htslib "$ours_signature" \
        "$tabix" --regions "$overlap_regions" --unique \
            --threads "$((threads - 1))" "$vcf_bgzf" >/dev/null
    ours_signature=$(capture_stdout query_targets rsomics "" \
        "$binary" tabix query "$vcf_bgzf" --index "$vcf_tbi" \
            --targets-file "$targets" --threads "$threads")
    capture_stdout query_targets htslib "$ours_signature" \
        "$tabix" --targets "$targets" --threads "$((threads - 1))" \
            "$vcf_bgzf" >/dev/null

    rm -f "$scratch/ours.tbi" "$scratch/ours.csi" "$scratch/ours-build.vcf.gz" \
        "$scratch/hts-build.vcf.gz" \
        "$scratch/hts-build.vcf.gz.tbi" "$scratch/hts-build.vcf.gz.csi"
    printf 'semantic and structural equality verified: %s\n' "$correctness"
}

prepare_output() {
    local workload=$1
    local tool=$2
    case "$workload" in
        compress_*) rm -f "$scratch/$tool.bgz" ;;
        build_tbi) rm -f "$scratch/$tool.tbi" ;;
        build_csi) rm -f "$scratch/$tool.csi" ;;
    esac
}

set_command() {
    local workload=$1
    local tool=$2
    command=()
    case "$workload:$tool" in
        compress_text_t1:rsomics) command=("$binary" bgzip "$vcf" --output "$scratch/rsomics.bgz" --threads 1 --compress-level 6) ;;
        compress_text_t1:htslib) command=("$bgzip" --output "$scratch/htslib.bgz" --threads 1 --compress-level 6 "$vcf") ;;
        compress_text_t4:rsomics) command=("$binary" bgzip "$vcf" --output "$scratch/rsomics.bgz" --threads "$threads" --compress-level 6) ;;
        compress_text_t4:htslib) command=("$bgzip" --output "$scratch/htslib.bgz" --threads "$threads" --compress-level 6 "$vcf") ;;
        compress_binary_t1:rsomics) command=("$binary" bgzip "$binary_input" --binary --output "$scratch/rsomics.bgz" --threads 1 --compress-level 6) ;;
        compress_binary_t1:htslib) command=("$bgzip" --binary --output "$scratch/htslib.bgz" --threads 1 --compress-level 6 "$binary_input") ;;
        compress_binary_t4:rsomics) command=("$binary" bgzip "$binary_input" --binary --output "$scratch/rsomics.bgz" --threads "$threads" --compress-level 6) ;;
        compress_binary_t4:htslib) command=("$bgzip" --binary --output "$scratch/htslib.bgz" --threads "$threads" --compress-level 6 "$binary_input") ;;
        decompress_text_t1:rsomics) command=("$binary" bgzip --decompress "$vcf_bgzf") ;;
        decompress_text_t1:htslib) command=("$bgzip" --decompress --stdout --threads 1 "$vcf_bgzf") ;;
        decompress_binary_t1:rsomics) command=("$binary" bgzip --decompress "$binary_bgzf") ;;
        decompress_binary_t1:htslib) command=("$bgzip" --decompress --stdout --threads 1 "$binary_bgzf") ;;
        partial_binary_512m:rsomics) command=("$binary" bgzip --decompress "$binary_bgzf" --index-input "$binary_gzi" --offset "$partial_offset" --size "$partial_size") ;;
        partial_binary_512m:htslib) command=("$bgzip" --decompress --stdout --index-name "$binary_gzi" --offset "$partial_offset" --size "$partial_size" "$binary_bgzf") ;;
        build_tbi:rsomics) command=("$binary" tabix build --preset vcf --output "$scratch/rsomics.tbi" "$scratch/rsomics-build.vcf.gz") ;;
        build_tbi:htslib) command=("$tabix" --force --preset vcf "$scratch/htslib-build.vcf.gz") ;;
        build_csi:rsomics) command=("$binary" tabix build --preset vcf --csi --output "$scratch/rsomics.csi" "$scratch/rsomics-build.vcf.gz") ;;
        build_csi:htslib) command=("$tabix" --force --preset vcf --csi "$scratch/htslib-build.vcf.gz") ;;
        query_sparse_t1:rsomics) command=("$binary" tabix query "$vcf_bgzf" --index "$vcf_tbi" --regions-file "$sparse_regions") ;;
        query_sparse_t1:htslib) command=("$tabix" --regions "$sparse_regions" "$vcf_bgzf") ;;
        query_dense_t4:rsomics) command=("$binary" tabix query "$vcf_bgzf" --index "$vcf_tbi" --regions-file "$dense_regions" --threads "$threads") ;;
        query_dense_t4:htslib) command=("$tabix" --regions "$dense_regions" --threads "$((threads - 1))" "$vcf_bgzf") ;;
        query_overlap_unique_t4:rsomics) command=("$binary" tabix query "$vcf_bgzf" --index "$vcf_tbi" --regions-file "$overlap_regions" --unique --threads "$threads") ;;
        query_overlap_unique_t4:htslib) command=("$tabix" --regions "$overlap_regions" --unique --threads "$((threads - 1))" "$vcf_bgzf") ;;
        query_targets_t4:rsomics) command=("$binary" tabix query "$vcf_bgzf" --index "$vcf_tbi" --targets-file "$targets" --threads "$threads") ;;
        query_targets_t4:htslib) command=("$tabix" --targets "$targets" --threads "$((threads - 1))" "$vcf_bgzf") ;;
        *) die "unknown workload/tool: $workload/$tool" ;;
    esac
}

execute_workload() {
    local workload=$1
    local tool=$2
    prepare_output "$workload" "$tool"
    set_command "$workload" "$tool"
    "${command[@]}" > /dev/null
}

measure() {
    local workload=$1
    local pair=$2
    local order=$3
    local tool=$4
    local timing="$scratch/timing"
    prepare_output "$workload" "$tool"
    set_command "$workload" "$tool"
    /usr/bin/time -p -l -o "$timing" "${command[@]}" > /dev/null
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$workload" "$pair" "$order" "$tool" \
        "$(awk '$1 == "real" { print $2 }' "$timing")" \
        "$(awk '$1 == "user" { print $2 }' "$timing")" \
        "$(awk '$1 == "sys" { print $2 }' "$timing")" \
        "$(awk '/maximum resident set size/ { print $1 }' "$timing")" >> "$raw_results"
}

write_summary() {
    summary="$result_directory/summary.tsv"
    paired="$result_directory/paired.tsv"
    printf 'workload\ttool\tmetric\tn\tmedian\tp99\tmean\tstdev\n' > "$summary"
    for workload in $workloads; do
        for tool in rsomics htslib; do
            for metric_column in wall_seconds:5 user_seconds:6 system_seconds:7 max_rss_bytes:8; do
                metric=${metric_column%%:*}
                column=${metric_column##*:}
                values="$scratch/values"
                awk -F '\t' -v workload="$workload" -v tool="$tool" -v column="$column" \
                    '$1 == workload && $4 == tool { print $column }' "$raw_results" \
                    | sort -n > "$values"
                awk -v workload="$workload" -v tool="$tool" -v metric="$metric" '
                    { value[NR] = $1; sum += $1; sumsq += $1 * $1 }
                    END {
                        median = NR % 2 ? value[(NR + 1) / 2] : (value[NR / 2] + value[NR / 2 + 1]) / 2
                        p99 = value[int(0.99 * NR + 0.999999)]
                        mean = sum / NR
                        variance = NR > 1 ? (sumsq - sum * sum / NR) / (NR - 1) : 0
                        if (variance < 0) variance = 0
                        stdev = sqrt(variance)
                        printf "%s\t%s\t%s\t%d\t%.9g\t%.9g\t%.9g\t%.9g\n", workload, tool, metric, NR, median, p99, mean, stdev
                    }
                ' "$values" >> "$summary"
            done
        done
    done

    printf 'workload\tpair\trsomics_wall_seconds\thtslib_wall_seconds\tspeedup\n' > "$paired"
    awk -F '\t' '
        NR > 1 { wall[$1, $2, $4] = $5; seen[$1, $2] = 1 }
        END {
            for (key in seen) {
                split(key, field, SUBSEP)
                workload = field[1]
                pair = field[2]
                ours = wall[workload, pair, "rsomics"]
                hts = wall[workload, pair, "htslib"]
                speedup = ours > 0 ? sprintf("%.9g", hts / ours) : "nan"
                printf "%s\t%s\t%.9g\t%.9g\t%s\n", workload, pair, ours, hts, speedup
            }
        }
    ' "$raw_results" | sort -t $'\t' -k1,1 -k2,2n >> "$paired"
}

write_provenance() {
    {
        printf 'utc=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
        uname -a
        sysctl -n hw.model
        sysctl -n machdep.cpu.brand_string
        printf 'physical_memory_bytes=%s\n' "$(sysctl -n hw.memsize)"
        sw_vers
        rustc --version --verbose
        "$binary" --version
        "$bgzip" --version | sed -n '1,2p'
        "$tabix" --version | sed -n '1,2p'
        printf 'git_head=%s\n' "$(git -C "$repository" rev-parse HEAD)"
        printf 'git_dirty=false\n'
        printf 'binary_build_manifest=%s\n' "$binary.build-provenance"
        printf 'records=%s\n' "$records"
        printf 'binary_bytes=%s\n' "$binary_bytes"
        printf 'threads=%s\n' "$threads"
        printf 'warmups=%s\n' "$warmups"
        printf 'runs=%s\n' "$runs"
        printf 'query_htslib_additional_threads=%s\n' "$((threads - 1))"
        shasum -a 256 "$binary" "$binary.build-provenance" "$bgzip" "$tabix" "$vcf" "$vcf_bgzf" \
            "$vcf_tbi" "$vcf_csi" "$binary_input" "$binary_bgzf" "$binary_gzi" \
            "$sparse_regions" "$dense_regions" "$overlap_regions" "$targets"
        wc -c "$vcf" "$vcf_bgzf" "$vcf_tbi" "$vcf_csi" \
            "$binary_input" "$binary_bgzf" "$binary_gzi" \
            "$sparse_regions" "$dense_regions" "$overlap_regions" "$targets"
    } > "$result_directory/provenance.txt"
}

run_benchmarks() {
    [[ $(uname -s) == Darwin ]] \
        || die "resource measurements are calibrated for macOS /usr/bin/time"
    ((threads >= 2)) || die "--threads must be at least 2 for the four-worker lanes"
    ((warmups >= 3)) || die "--warmups must be at least 3"
    ((runs >= 10)) || die "--runs must be at least 10"
    [[ -z $(find "$result_directory" -mindepth 1 -maxdepth 1 -print -quit) ]] \
        || die "full benchmark result directory must be empty: $result_directory"
    smoke

    scratch="$result_directory/scratch"
    cp "$vcf_bgzf" "$scratch/rsomics-build.vcf.gz"
    cp "$vcf_bgzf" "$scratch/htslib-build.vcf.gz"
    raw_results="$result_directory/raw.tsv"
    printf 'workload\tpair\torder\ttool\twall_seconds\tuser_seconds\tsystem_seconds\tmax_rss_bytes\n' \
        > "$raw_results"
    workloads='compress_text_t1 compress_text_t4 compress_binary_t1 compress_binary_t4 decompress_text_t1 decompress_binary_t1 partial_binary_512m build_tbi build_csi query_sparse_t1 query_dense_t4 query_overlap_unique_t4 query_targets_t4'

    local workload pair
    for workload in $workloads; do
        printf 'warmup %s\n' "$workload"
        for ((pair = 1; pair <= warmups; pair++)); do
            if ((pair % 2)); then
                execute_workload "$workload" rsomics
                execute_workload "$workload" htslib
            else
                execute_workload "$workload" htslib
                execute_workload "$workload" rsomics
            fi
        done
        printf 'measure %s\n' "$workload"
        for ((pair = 1; pair <= runs; pair++)); do
            if ((pair % 2)); then
                measure "$workload" "$pair" 1 rsomics
                measure "$workload" "$pair" 2 htslib
            else
                measure "$workload" "$pair" 1 htslib
                measure "$workload" "$pair" 2 rsomics
            fi
        done
        rm -f "$scratch/rsomics.bgz" "$scratch/htslib.bgz" \
            "$scratch/rsomics.tbi" "$scratch/htslib-build.vcf.gz.tbi" \
            "$scratch/rsomics.csi" "$scratch/htslib-build.vcf.gz.csi"
    done
    write_summary
    write_provenance
    shasum -a 256 "$raw_results" "$result_directory/summary.tsv" \
        "$result_directory/paired.tsv" "$result_directory/correctness.tsv" \
        "$result_directory/provenance.txt" > "$result_directory/RESULTS.SHA256"
    rm -f "$scratch/timing" "$scratch/values" "$scratch/rsomics-build.vcf.gz" \
        "$scratch/htslib-build.vcf.gz"
    printf 'benchmark complete: %s\n' "$result_directory"
}

summarize() {
    local summary="$result_directory/summary.tsv"
    local paired="$result_directory/paired.tsv"
    [[ -f "$summary" && -f "$paired" && -f "$result_directory/RESULTS.SHA256" ]] \
        || die "benchmark results are incomplete"
    shasum -a 256 --check "$result_directory/RESULTS.SHA256" >/dev/null \
        || die "benchmark result checksum verification failed"
    printf '# rsomics-index performance summary\n\n'
    printf '| Workload | rsomics median s | HTSlib median s | Speedup | rsomics median RSS MiB | HTSlib median RSS MiB |\n'
    printf '|---|---:|---:|---:|---:|---:|\n'
    awk -F '\t' '
        NR == FNR {
            if (FNR > 1 && $3 == "wall_seconds") wall[$1, $2] = $5
            if (FNR > 1 && $3 == "max_rss_bytes") rss[$1, $2] = $5
            next
        }
        FNR == 1 { next }
        !seen[$1]++ {
            speedup = wall[$1, "rsomics"] > 0 \
                ? wall[$1, "htslib"] / wall[$1, "rsomics"] \
                : 0
            printf "| `%s` | %.3f | %.3f | %.2fx | %.1f | %.1f |\n", $1,
                wall[$1, "rsomics"], wall[$1, "htslib"],
                speedup,
                rss[$1, "rsomics"] / 1048576, rss[$1, "htslib"] / 1048576
        }
    ' "$summary" "$paired"
    printf '\nRaw evidence: `%s`\n' "$result_directory"
}

(($# >= 1)) || usage
mode=$1
shift
parse_options "$@"

case "$mode" in
    build) build_binary ;;
    generate) prepare; generate ;;
    smoke) prepare; smoke ;;
    run) prepare; run_benchmarks ;;
    summarize)
        mkdir -p "$result_directory"
        result_directory=$(realpath "$result_directory")
        external_path "$result_directory"
        summarize
        ;;
    *) usage ;;
esac
