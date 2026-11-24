"""Repository hygiene over the tracked files (`just hygiene`, part of `just check`).

Fails when Python bytecode is tracked: it is built output of one interpreter,
and `.gitignore` keeps new copies out.

Warns, without failing, when a tracked file is read-only in this worktree.
The binary writes journal events and retained blobs read-only, so a fixture
copied from a workspace it wrote keeps that mode. Git records no write bit,
so the commit is fine, but a later checkout that has to replace the file in
this worktree fails part-way (maw: "IO error while writing blob") and leaves
it half checked out. That is a fact of this worktree, not of the repository,
so it is a warning with the fix.
"""

import os
import re
import stat
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BYTECODE = re.compile(r"(^|/)__pycache__/|\.py[co]$")


def tracked():
    listing = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT)
    return [name for name in listing.decode("utf-8", "surrogateescape").split("\0") if name]


def read_only(names):
    for name in names:
        try:
            mode = os.lstat(ROOT / name).st_mode
        except FileNotFoundError:
            continue
        if stat.S_ISREG(mode) and not mode & stat.S_IWUSR:
            yield name


if __name__ == "__main__":
    names = tracked()
    bytecode = [name for name in names if BYTECODE.search(name)]
    for name in bytecode:
        print(f"{name}: tracked Python bytecode; remove it with `git rm --cached -- {name}`",
              file=sys.stderr)
    locked = list(read_only(names))
    if locked:
        print(f"warning: {len(locked)} tracked file(s) are read-only in this worktree, so a checkout "
              "that replaces them fails part-way:", file=sys.stderr)
        for name in locked:
            print(f"  {name}", file=sys.stderr)
        print("fix: chmod u+w -- " + " ".join(locked), file=sys.stderr)
    sys.exit(1 if bytecode else 0)
