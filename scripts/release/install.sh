#!/bin/sh
# Install the investigation release package into one prefix.
#
#   ./install.sh --prefix DIR [--alias-inv] [--replace]
#
# Copies bin/investigation to DIR/bin/investigation after checking every file
# of the package against SHA256SUMS. Nothing outside DIR changes: no shell
# start-up file, no PATH entry, no other directory.
#
# `inv` is only an optional alias. With --alias-inv the script adds
# DIR/bin/inv, a symbolic link to investigation, only when no `inv` exists in
# DIR/bin and none is found on PATH; otherwise it leaves every existing `inv`
# untouched, says which one it kept, and still succeeds. It never overwrites
# or removes an `inv`.
#
# An existing DIR/bin/investigation is refused unless --replace is given; a
# symbolic link there is always refused.
set -eu

usage() {
    echo "Usage: ./install.sh --prefix DIR [--alias-inv] [--replace]" >&2
    exit 2
}

prefix=
alias_inv=no
replace=no
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] || usage
            prefix=$2
            shift 2
            ;;
        --alias-inv) alias_inv=yes; shift ;;
        --replace) replace=yes; shift ;;
        -h | --help)
            sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) usage ;;
    esac
done
[ -n "$prefix" ] || usage
case "$prefix" in
    /*) ;;
    *) prefix=$(pwd)/$prefix ;;
esac

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

# Check every packaged file before copying anything.
if command -v sha256sum >/dev/null 2>&1; then
    if ! sha256sum --check --quiet --strict SHA256SUMS; then
        echo "install.sh: a packaged file does not match SHA256SUMS; nothing was installed." >&2
        exit 1
    fi
else
    echo "install.sh: sha256sum is not available, so the package cannot be checked; nothing was installed." >&2
    exit 1
fi

bin=$prefix/bin
target=$bin/investigation
if [ -L "$target" ]; then
    echo "install.sh: $target is a symbolic link; nothing was installed." >&2
    exit 1
fi
if [ -e "$target" ] && [ "$replace" != yes ]; then
    echo "install.sh: $target exists; pass --replace to replace it. Nothing was installed." >&2
    exit 1
fi

mkdir -p "$bin"
staged=$bin/.investigation.install.$$
trap 'rm -f "$staged"' EXIT INT TERM
cp bin/investigation "$staged"
chmod 0755 "$staged"
mv -f "$staged" "$target"
trap - EXIT INT TERM
echo "installed $target"

if [ "$alias_inv" = yes ]; then
    found=$(command -v inv 2>/dev/null || true)
    if [ -e "$bin/inv" ] || [ -L "$bin/inv" ]; then
        echo "kept the existing $bin/inv; the inv alias was not added"
    elif [ -n "$found" ]; then
        echo "kept the existing inv on PATH ($found); the inv alias was not added"
    else
        ln -s investigation "$bin/inv"
        echo "added $bin/inv -> investigation"
    fi
fi

case ":${PATH:-}:" in
    *":$bin:"*) ;;
    *) echo "add $bin to PATH to run investigation by name" ;;
esac
"$target" version --format json >/dev/null
echo "Next: $target onboard --format json"
