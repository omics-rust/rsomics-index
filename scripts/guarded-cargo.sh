#!/bin/bash
set -euo pipefail

repository=$(cd "$(dirname "$0")/.." && pwd -P)
export CARGO_HOME=/Volumes/KIOXIA/Developments/cargo-home
export CARGO_TARGET_DIR=/Volumes/KIOXIA/Developments/cargo-target/rsomics-index
export RUSTUP_HOME=/Volumes/KIOXIA/Developments/rustup-home
export TMPDIR=/Volumes/KIOXIA/Developments/tmp
export XDG_CACHE_HOME=/Volumes/KIOXIA/Developments/tmp/cache
export GH_CACHE_DIR=/Volumes/KIOXIA/Developments/tmp/gh-cache
export CLANG_MODULE_CACHE_PATH=/Volumes/KIOXIA/Developments/tmp/clang-modules-index
export CCACHE_DIR=/Volumes/KIOXIA/Developments/tmp/ccache-index
export SCCACHE_DIR=/Volumes/KIOXIA/Developments/tmp/sccache-index
export PYTHONDONTWRITEBYTECODE=1
toolchain="$RUSTUP_HOME/toolchains/1.91.0-aarch64-apple-darwin/bin"
export CARGO="$toolchain/cargo" RUSTC="$toolchain/rustc"
export RUSTDOC="$toolchain/rustdoc" RUSTFMT="$toolchain/rustfmt"
export PATH="$toolchain:$PATH"
export CARGO_BUILD_JOBS=2

for argument in "$@"; do
    case "$argument" in
        --target-dir|--target-dir=*|--config|--config=*)
            echo "Storage overrides are not supported: $argument" >&2; exit 1 ;;
    esac
done
for name in CARGO_BUILD_TARGET CARGO_BUILD_BUILD_DIR RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER; do
    if [[ -n "${!name:-}" ]]; then
        echo "Unsupported compiler/target override: $name" >&2; exit 1
    fi
done

paths=("$repository" "$CARGO_HOME" "$CARGO_TARGET_DIR" "$RUSTUP_HOME"
    "$TMPDIR" "$toolchain" "$XDG_CACHE_HOME" "$GH_CACHE_DIR"
    "$CLANG_MODULE_CACHE_PATH" "$CCACHE_DIR" "$SCCACHE_DIR")
check_paths=false
if [[ "${1:-}" == --check-paths ]]; then
    check_paths=true
    shift
    for path in "$@"; do paths[${#paths[@]}]="$path"; done
fi
/usr/bin/python3 - "${paths[@]}" <<'PY'
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys

roots = [Path('/Volumes/KIOXIA'), Path("/Volumes/Zane's HDD")]
devices = {}
for root in roots:
    info = plistlib.loads(subprocess.check_output(
        ['/usr/sbin/diskutil', 'info', '-plist', str(root)]))
    if info.get('MountPoint') != str(root) or info.get('Internal') is not False:
        sys.exit(f'Not an externally mounted volume: {root}')
    devices[root] = root.stat().st_dev
    if devices[root] == Path('/').stat().st_dev:
        sys.exit(f'Project volume resolves to boot storage: {root}')
    free = shutil.disk_usage(root).free
    if free < 8 * 1024**3:
        sys.exit(f'Less than 8 GiB external headroom: {root}')
    print(f'External volume: {root}, device={info["DeviceNode"]}, free={free}')
for raw in sys.argv[1:]:
    path = Path(raw).resolve()
    root = next((r for r in roots if path == r or r in path.parents), None)
    existing = path
    while not existing.exists():
        existing = existing.parent
    if root is None or existing.stat().st_dev != devices[root]:
        sys.exit(f'Forbidden project path: {raw} -> {path}')
    print(f'External path: {path}')
PY
mkdir -p "$CARGO_TARGET_DIR" "$XDG_CACHE_HOME" "$GH_CACHE_DIR" \
    "$CLANG_MODULE_CACHE_PATH" "$CCACHE_DIR" "$SCCACHE_DIR"
target=$(cd "$repository" && "$CARGO" metadata --offline --locked --no-deps \
    --format-version 1 | /usr/bin/python3 -c \
    'import json,sys; print(json.load(sys.stdin)["target_directory"])')
test "$target" = "$CARGO_TARGET_DIR"
echo "Cargo resolved target: $target"
if [[ "$check_paths" == true ]]; then
    exit 0
fi
case "${1:-}" in
    --exec) shift; exec "$@" ;;
    clippy) shift; exec "$toolchain/cargo-clippy" clippy "$@" ;;
    fmt) shift; exec "$toolchain/cargo-fmt" fmt "$@" ;;
    *) exec "$CARGO" "$@" ;;
esac
