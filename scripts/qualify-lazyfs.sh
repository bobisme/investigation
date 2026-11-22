#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 ]]; then
    echo "Usage: bash scripts/qualify-lazyfs.sh PATH/TO/LAZYFS_BINARY" >&2
    exit 2
fi
if [[ $(uname -s) != Linux || ! -c /dev/fuse || ! -r /dev/fuse || ! -w /dev/fuse ]]; then
    echo "LazyFS qualification needs accessible /dev/fuse on Linux. No test ran." >&2
    exit 1
fi
for tool in fusermount3 mountpoint findmnt python3; do
    command -v "$tool" >/dev/null || { echo "Missing tool: $tool" >&2; exit 1; }
done
lazyfs=$(realpath -- "$1")
[[ -x "$lazyfs" ]] || { echo "Provide a built LazyFS executable" >&2; exit 2; }

scratch=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/investigation-lazyfs.XXXXXX")
pid=
cleanup() {
    result=$?
    trap - EXIT
    if mountpoint -q -- "$scratch/mount"; then
        if ! fusermount3 -u -- "$scratch/mount"; then
            echo "Clean unmount failed. Kept scratch directory: $scratch" >&2
            exit 1
        fi
    fi
    if [[ -n "$pid" ]]; then
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
        wait "$pid" 2>/dev/null || true
    fi
    if (( result != 0 )) && [[ -f "$scratch/lazyfs.console" ]]; then
        tail -n 80 -- "$scratch/lazyfs.console" >&2
    fi
    rm -r -- "$scratch"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -- "$scratch/backing" "$scratch/mount"
uname -sr
fusermount3 --version
findmnt --target "$scratch/backing" --noheadings --output FSTYPE,OPTIONS

python3 - "$scratch" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
config = """[faults]
fifo_path = {command}
fifo_path_completed = {completion}

[cache]
apply_eviction = false

[cache.simple]
custom_size = "32MB"
blocks_per_page = 1

[filesystem]
log_all_operations = false
logfile = {log}
"""

def quote(path):
    return json.dumps(str(root / path), ensure_ascii=False)

(root / "lazyfs.toml").write_text(config.format(
    command=quote("faults.fifo"),
    completion=quote("completed.fifo"),
    log=quote("lazyfs.log"),
), encoding="utf-8")
PY

INV_LAZYFS_TEST_ROOT="$scratch/backing" cargo test --locked -p investigation-store --lib \
    'store::real_crash_tests::lazyfs_seed' -- --exact --ignored --nocapture

"$lazyfs" "$scratch/mount" --config-path "$scratch/lazyfs.toml" \
    -o modules=subdir -o "subdir=$scratch/backing" -s -f >"$scratch/lazyfs.console" 2>&1 &
pid=$!
ready=false
for (( attempt=0; attempt < 200; attempt++ )); do
    if mountpoint -q -- "$scratch/mount" && [[ -p "$scratch/faults.fifo" && -p "$scratch/completed.fifo" ]]; then
        ready=true
        break
    fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.05
done
if [[ "$ready" != true ]]; then
    echo "LazyFS mount and its completion FIFO did not become ready." >&2
    exit 1
fi

findmnt --target "$scratch/mount" --noheadings --output FSTYPE
INV_LAZYFS_TEST_ROOT="$scratch/mount" \
INV_LAZYFS_TEST_BACKING="$scratch/backing" \
INV_LAZYFS_TEST_CONTROL_DIR="$scratch" \
    cargo test --locked -p investigation-store --lib \
    'store::real_crash_tests::lazyfs_publication_boundaries_and_retry' -- --exact --ignored --nocapture
