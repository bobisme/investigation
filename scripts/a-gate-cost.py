"""Read the participant syscall trace and count bytes by their source."""

from collections import Counter
from pathlib import Path
import re


SYSCALLS = "execve,read,readv,pread64,write,writev,pwrite64,openat,copy_file_range,sendfile,splice"
LINE = re.compile(r"^(\d+)\s+(.*)$")
RESUMED = re.compile(r"^<\.\.\. [a-z0-9_]+ resumed>(.*)$")
RESULT = re.compile(r"\s=\s(\d+)(?:<[^>]+>)?(?:\s.*)?$")
FD = re.compile(r"(?<![\w.-])(\d+)<([^>]+)>")
COMMAND = re.compile(r'^execve\("/tools/investigation", \[(.*?)\],')
SOURCE = {"/work/source.txt", "/work/update.txt"}


def journal_events(work):
    return {path.relative_to(work).as_posix() for path in work.rglob("state/cases/*/events/*.json")
            if path.is_file() and not path.is_symlink()}


def events(lines):
    pending = {}
    for line in lines:
        match = LINE.match(line.rstrip())
        if not match:
            continue
        pid, call = match.groups()
        if call.endswith("<unfinished ...>"):
            pending[pid] = call.removesuffix("<unfinished ...>")
            continue
        resumed = RESUMED.match(call)
        if resumed:
            if pid not in pending:
                continue
            call = pending.pop(pid) + resumed.group(1)
        value = RESULT.search(call)
        if value:
            yield pid, call, int(value.group(1))


def is_note(filename):
    return filename.startswith("/work/") and Path(filename).suffix.lower() in {".md", ".markdown"}


def scope(filename, work):
    if filename.startswith(str(work) + "/"):
        return "/work/" + filename.removeprefix(str(work) + "/")
    return filename


def trace_cost(lines, work):
    counters = Counter()
    inv = {}
    for pid, call, value in events(lines):
        name = call.split("(", 1)[0]
        if name == "execve":
            invocation = COMMAND.match(call)
            if invocation is None:
                inv.pop(pid, None)
                continue
            argv = invocation.group(1)
            selectors = set(re.findall(r'"([^"\n]+)"', argv))
            discovery = bool(selectors & {"schema", "onboard", "help", "--help", "-h"})
            evidence = {"evidence", "read"} <= selectors
            inv[pid] = (discovery, evidence, "onboard" in selectors)
            counters["discovery_calls"] += int(discovery)
            counters["onboard_calls"] += int(inv[pid][2])
        elif value > 0 and name in {"read", "readv", "pread64", "write", "writev", "pwrite64",
                                     "copy_file_range", "sendfile", "splice"}:
            descriptors = [(int(number), scope(filename, work)) for number, filename in FD.findall(call)]
            if not descriptors:
                continue
            if name in {"copy_file_range", "splice", "sendfile"} and len(descriptors) >= 2:
                reader, writer = descriptors[:2]
                if name == "sendfile":
                    reader, writer = writer, reader
                readers, writers = [reader], [writer]
            elif name in {"read", "readv", "pread64"}:
                readers, writers = descriptors[:1], []
            else:
                readers, writers = [], descriptors[:1]
            for _, filename in readers:
                counters["note_read_bytes"] += value if is_note(filename) else 0
                counters["evidence_bytes"] += value if filename in SOURCE else 0
            for fd, filename in writers:
                counters["note_write_bytes"] += value if is_note(filename) else 0
                if fd == 1 and pid in inv:
                    discovery, evidence, _ = inv[pid]
                    counters["discovery_bytes"] += value if discovery else 0
                    counters["evidence_bytes"] += value if evidence else 0
        elif name == "openat" and pid not in inv and re.search(r"\bO_(?:WRONLY|RDWR)\b", call):
            filename = re.search(r'^openat\([^,]+, "([^"]+\.jsonl?)",', call)
            if filename and (not filename.group(1).startswith("/") or filename.group(1).startswith("/work/")):
                counters["preparation_calls"] = 1
    return dict(counters)
