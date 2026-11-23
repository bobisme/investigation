#!/usr/bin/env python3
"""Cross-check the syscall counters against hand-counted bytes and calls."""

from pathlib import Path
import runpy
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
COST = runpy.run_path(str(ROOT / "scripts/a-gate-cost.py"))


def synthetic():
    source = [
        '10 execve("/tools/investigation", ["investigation", "onboard"], 0x0) = 0',
        '10 write(1<pipe:[1]>, "hello", 5) = 5',
        '11 read(3</work/source.txt>,  <unfinished ...>',
        '12 write(3</work/NOTES.md>, "draft", 5) = 5',
        '11 <... read resumed>"data", 4) = 4',
        '12 copy_file_range(3</work/NOTES.md>, NULL, 4</tmp/out>, NULL, 5, 0) = 5',
        '11 openat(AT_FDCWD</work>, "command.json", O_WRONLY|O_CREAT, 0666) = 4</work/command.json>',
        '10 openat(AT_FDCWD</work>, "state/cases/event.json", O_WRONLY|O_CREAT, 0666) = 4',
    ]
    assert COST["trace_cost"](source, Path("/tmp/work")) == {
        "discovery_calls": 1, "onboard_calls": 1, "discovery_bytes": 5,
        "evidence_bytes": 4, "note_read_bytes": 5, "note_write_bytes": 5, "preparation_calls": 1,
    }


def live(binary):
    runner = runpy.run_path(str(ROOT / "scripts/run-a-gate.py"))
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory).resolve()
        work = root / "work"
        work.mkdir()
        (work / "source.txt").write_bytes(b"correction\n")
        direct = runner["shell"]("investigation --help", work, binary, 5)
        assert direct["exit"] == 0
        trace = root / "trace.log"
        result = runner["shell"](
            "printf 'note' > NOTES.md; cat source.txt NOTES.md; printf '{}' > txn.json; investigation --help",
            work, binary, 5, trace,
        )
        with trace.open() as lines:
            observed = {key: value for key, value in COST["trace_cost"](lines, work).items() if value}
        assert result["exit"] == 0 and not result["truncated"], result
        assert observed == {
            "note_write_bytes": 4, "evidence_bytes": 11, "note_read_bytes": 4,
            "preparation_calls": 1, "discovery_calls": 1, "discovery_bytes": direct["bytes"],
        }, observed
        assert result["bytes"] == direct["bytes"] + 15
        result = runner["shell"](
            "investigation init casework --no-vault --json && printf 'source text' | "
            "investigation --workspace casework --actor agent:rehearsal start 'Test receipt' "
            "--attach - --command-id test --json",
            work, binary, 5,
        )
        assert result["exit"] == 0 and len(COST["journal_events"](work)) == 1, result


if __name__ == "__main__":
    synthetic()
    if len(sys.argv) == 2:
        live(Path(sys.argv[1]).resolve(strict=True))
    print("A-gate cost accounting checks passed.")
