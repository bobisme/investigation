#!/usr/bin/env python3
"""Release-trial tasks 8-10 of plan §20.5: set up, grade and rehearse.

- Task 8, layered reads: a successor inspects a case of at least 40 records
  and answers in the four A-gate fields. Graded on the journal (no promotion),
  the per-depth byte and call counts of its reads, its raw evidence reads and
  whether the answer keeps the decisive caveat.
- Task 9, blindspots plus a novel risk: a reviewer finds a seeded structural
  gap, records a semantic risk the checker cannot see and does not promote
  the case.
- Task 10, bounded retrospective: a closer records a Retrospective of a failed
  or inconclusive case and closes it in one transaction, inventing no failure
  and changing no method pack, pin or lesson admission.

An instance file (`training/trial-tasks/tasks-8-10/development/*.json`, or a sealed
file outside the repository) names its task and holds a setup, a key and a
rehearsal. The setup and rehearsal steps use the drill step format of
`crates/investigation-cli/tests/drills_3_4.rs`; `{"drill": FILE}` reuses a
drill file's steps before its first `response` step. Generated IDs are never
pinned: keys name records through setup bindings and titles.

Usage:
  trial-tasks-8-10.py setup INSTANCE OUT --bin BIN [--arm inv|notes]
  trial-tasks-8-10.py grade OUT --bin BIN [--calls CALLS.jsonl] [--answer ANSWER.json]
  trial-tasks-8-10.py calls TRACE WORK            # syscall trace -> call log (JSON lines)
  trial-tasks-8-10.py self-check --bin BIN [INSTANCE ...]

`setup` writes OUT/work (the participant directory) and OUT/evaluator (the
instance, its key and the baseline; never mount it into a run). `grade`
prints deterministic check results, measures and the packet for the judgment
rubric. `self-check` sets up every instance, plays its scripted correct path
(every check passes) and each scripted fault (exactly the named checks fail).
"""

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import copy
import hashlib
import json
from pathlib import Path
import re
import runpy
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
TASKS = ROOT / "training/trial-tasks/tasks-8-10"
DEVELOPMENT = TASKS / "development"
SPEC = TASKS / "tasks-8-10.json"
DRILLS = ROOT / "fixtures/adversarial-drills"
GOLDENS = ROOT / "fixtures/vault-goldens"
GRADER = runpy.run_path(str(ROOT / "scripts/grade-a-gate.py"))
COST = runpy.run_path(str(ROOT / "scripts/a-gate-cost.py"))
# Directories a checked-in workspace needs; git keeps no empty directory.
REQUIRED = ["state/cases", "state/events", "state/blobs/sha256", "state/checkpoints", "state/staging",
            "state/locks", "state/tmp", "cache", "local", "exports", "vault/Notes", "vault/Boards", "vault/Inbox"]
PROMOTABLE = ("concl_", "dec_")
FAILURE_CATEGORIES = {"run.invalid", "run.failed", "observation.invalid"}
GLOBAL_VALUE_OPTIONS = {"--workspace", "--case", "--actor", "--session", "--format"}
DISCOVERY = {"schema", "onboard", "docs", "guide", "method", "help"}
WRITES = {"record", "ingest", "start", "observe", "note", "case", "source", "capture"}
LAYERED = {"brief.compact", "brief.standard", "brief.delta", "show.title", "show.summary", "frontier", "next",
           "search", "blindspots", "receipt"}
DEEP = {"brief.deep", "show.full", "explain", "review-packet", "retrospective"}


def unlabelled(raw):
    """A blindspots audit without the label a historical re-read adds: the same
    audit the head gave at that snapshot, field for field."""
    try:
        audit = json.loads(raw)
    except ValueError:
        return None
    if isinstance(audit, dict):
        audit.pop("historical", None)
        audit.pop("historical_read", None)
    return audit


class Failed(Exception):
    pass


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def body_digest(body):
    return digest(json.dumps(body, sort_keys=True, separators=(",", ":")).encode())


# ---------------------------------------------------------------- the binary


class Inv:
    """Runs the binary in one workspace with an empty PATH and logs every call."""

    def __init__(self, binary, root):
        self.binary = str(Path(binary).resolve())
        self.root = Path(root)
        self.calls = []

    def raw(self, args, stdin=None):
        process = subprocess.run(
            [self.binary, *args], cwd=self.root, input=None if stdin is None else json.dumps(stdin).encode(),
            stdin=None if stdin is not None else subprocess.DEVNULL, capture_output=True,
            env={"PATH": "/nonexistent"}, check=False)
        self.calls.append({"argv": ["investigation", *args], "stdout_bytes": len(process.stdout),
                           "exit": process.returncode})
        return process.returncode, process.stdout.decode()

    def json(self, args, stdin=None, exit=0):
        args = [*args, "--format", "json"]
        code, stdout = self.raw(args, stdin)
        try:
            value = json.loads(stdout)
        except json.JSONDecodeError as error:
            raise Failed(f"{args}: {error}: {stdout[:2000]}") from None
        if code != exit:
            raise Failed(f"{args} exited {code}, wanted {exit}: {stdout[:4000]}")
        return value, stdout


# ---------------------------------------------------------------- the journal


def events(root):
    """Every journal event of every case, in case and sequence order."""
    found = []
    for case in sorted((Path(root) / "state/cases").glob("*/events")):
        for path in sorted(case.glob("*.json")):
            found.append(read_json(path))
    return found


def records(root, case=None, through=None):
    """Current revision of every record, optionally of one case up to a sequence."""
    current = {}
    for event in events(root):
        accepted = event["accepted_operations"]
        if case is not None and accepted["case_id"] != case:
            continue
        if through is not None and event["sequence"] > through:
            continue
        for record in accepted["records"]:
            current[record["header"]["id"]] = record
    return current


def heads(root):
    return {event["accepted_operations"]["case_id"]: event["sequence"] for event in events(root)}


def after(root, baseline):
    """Events committed after setup: the participant's transactions."""
    setup = baseline["heads"]
    return [event for event in events(root)
            if event["sequence"] > setup.get(event["accepted_operations"]["case_id"], 0)]


def operations(event):
    return event["request"]["input"]["operations"]


def created_ids(event):
    return [record["header"]["id"] for record in event["accepted_operations"]["records"]
            if record["header"]["revision"] == 1]


def fingerprint(root):
    return {identifier: [record["header"]["revision"], body_digest(record["body"])]
            for identifier, record in records(root).items()}


# ---------------------------------------------------------------- drill steps


def walk(value, segments, path):
    if not segments:
        return value
    segment, rest = segments[0], segments[1:]
    if segment == "*":
        return [walk(item, rest, path) for item in value]
    if segment.startswith("[") and segment.endswith("]"):
        key, wanted = segment[1:-1].split("=", 1)
        found = [item for item in value if isinstance(item, dict) and item.get(key) == wanted]
        if len(found) != 1:
            raise Failed(f"{path}: {segment} matched {len(found)}")
        return walk(found[0], rest, path)
    if isinstance(value, list) and segment.isdigit() and int(segment) < len(value):
        return walk(value[int(segment)], rest, path)
    if isinstance(value, dict) and segment in value:
        return walk(value[segment], rest, path)
    raise Failed(f"{path}: no {segment!r}")


def pointer_get(document, pointer):
    """The value at a JSON pointer, or None when it is absent."""
    for part in pointer.split("/")[1:]:
        part = part.replace("~1", "/").replace("~0", "~")
        if isinstance(document, list) and part.isdigit() and int(part) < len(document):
            document = document[int(part)]
        elif isinstance(document, dict):
            document = document.get(part)
        else:
            return None
    return document


def exact(value):
    if isinstance(value, dict) and isinstance(value.get("id"), str) and isinstance(value.get("revision"), int):
        return {"id": value["id"], "revision": value["revision"]}
    return value


class Replay:
    """Interprets the drill step format through the public binary."""

    def __init__(self, inv, name, bindings=None):
        self.inv = inv
        self.name = name
        self.bindings = dict(bindings or {})
        self.audits = {}
        self.step = 0
        self.answer = None

    def lookup(self, path):
        path = self.interpolate(path)
        segments = path.split("/")
        if segments[0] not in self.bindings:
            raise Failed(f"{self.name} step {self.step}: unbound {path}")
        return walk(self.bindings[segments[0]], segments[1:], path)

    def interpolate(self, path):
        def value(match):
            found = self.lookup(match.group(1))
            if not isinstance(found, str):
                raise Failed(f"{path} reads {found}")
            return found
        return re.sub(r"\{([^{}]+)\}", value, path)

    def resolve(self, template):
        if isinstance(template, dict):
            if set(template) == {"$"}:
                return copy.deepcopy(self.lookup(template["$"]))
            if set(template) == {"$concat"}:
                joined = []
                for part in template["$concat"]:
                    part = self.resolve(part)
                    joined.extend(part if isinstance(part, list) else [part])
                return joined
            if set(template) == {"$merge"}:
                merged = {}
                for part in template["$merge"]:
                    merged.update(self.resolve(part))
                return merged
            return {key: self.resolve(value) for key, value in template.items()}
        if isinstance(template, list):
            return [self.resolve(item) for item in template]
        return template

    def case_id(self):
        return self.lookup("case/id")

    def expect(self, response, expect):
        for pointer, wanted in (expect or {}).items():
            got = pointer_get(response, pointer)
            if got != self.resolve(wanted):
                raise Failed(f"{self.name} step {self.step}: {pointer} is {got!r}, wanted {self.resolve(wanted)!r}")

    def keep(self, step, value):
        if step.get("as"):
            self.bindings[step["as"]] = value

    def find(self, specs):
        case = self.bindings.get("case", {}).get("id")
        current = records(self.inv.root, case)
        for name, spec in specs.items():
            prefix = spec["kind"] + "_"
            where = {pointer: self.resolve(value) for pointer, value in spec.get("where", {}).items()}
            found = [{"id": identifier, "revision": record["header"]["revision"]}
                     for identifier, record in sorted(current.items())
                     if identifier.startswith(prefix)
                     and all(pointer_get(record["body"], pointer) == value for pointer, value in where.items())]
            if spec.get("all"):
                if not found:
                    raise Failed(f"{self.name} step {self.step}: none for {name}")
                self.bindings[name] = found
            elif len(found) != 1:
                raise Failed(f"{self.name} step {self.step}: {name} matched {len(found)}")
            else:
                self.bindings[name] = found[0]

    def record(self, step):
        request = step["record"]
        case = self.resolve(request["case"]) if "case" in request else self.case_id()
        command = {
            "schema_version": "inv.command.v1", "command_id": f"{self.name}-{self.step}", "case_id": case,
            "actor": request["actor"], "preconditions": self.resolve(request.get("preconditions", [])),
            "operations": self.resolve(request["operations"]),
        }
        if "actor_descriptor" in request:
            command["actor_descriptor"] = request["actor_descriptor"]
        receipt, _ = self.inv.json(["record", "--input", "-"], command, step.get("exit", 0))
        if step.get("exit", 0) == 0 and receipt.get("result") != "committed":
            raise Failed(f"{self.name} step {self.step}: {receipt}")
        for name, pointer in step.get("bind", {}).items():
            self.bindings[name] = exact(walk(receipt, pointer.split("/")[1:], pointer))
        self.expect(receipt, step.get("expect"))
        self.keep(step, receipt)

    def cli(self, step):
        args = self.resolve(step["cli"])
        stdin = self.resolve(step["stdin"]) if "stdin" in step else None
        if step.get("text"):
            code, _ = self.inv.raw(args)
            if code != step.get("exit", 0):
                raise Failed(f"{self.name} step {self.step}: {args} exited {code}")
            return
        response, _ = self.inv.json(args, stdin, step.get("exit", 0))
        self.expect(response, step.get("expect"))
        self.keep(step, response)

    def audit(self, step):
        args = ["--case", self.case_id(), "blindspots"]
        if "subject" in step["audit"]:
            args += ["--subject", self.resolve(step["audit"]["subject"])]
        response, raw = self.inv.json(args)
        expect = step.get("expect", {})
        if "outcome" in expect and response["outcome"] != expect["outcome"]:
            raise Failed(f"{self.name} step {self.step}: outcome {response['outcome']}")
        if "check_ids" in expect:
            found = sorted(check["check_id"] for check in response["checks"])
            if found != expect["check_ids"]:
                raise Failed(f"{self.name} step {self.step}: check IDs {found}")
        for wanted in expect.get("findings", []):
            wanted = self.resolve(wanted)
            hits = [check for check in response["checks"] if check["check_id"] == wanted["check_id"]
                    and ("subject_refs" not in wanted or check["subject_refs"] == wanted["subject_refs"])]
            if len(hits) != 1:
                raise Failed(f"{self.name} step {self.step}: {wanted['check_id']} matched {len(hits)}")
        if step.get("as"):
            self.audits[step["as"]] = raw
        self.keep(step, response)

    def surface(self, step):
        response, _ = self.inv.json(["--case", self.case_id(), step["surface"]])
        self.keep(step, response)

    def traverse(self, step):
        """Read every record of the case at one depth (a rehearsal of a full traversal)."""
        for identifier in sorted(records(self.inv.root, self.case_id())):
            self.inv.json(["--case", self.case_id(), "show", identifier, "--depth", step["traverse"]])

    def run(self, steps, start=0):
        for index, step in enumerate(steps):
            self.step = start + index
            if "find" in step:
                self.find(step["find"])
            elif "record" in step:
                self.record(step)
            elif "cli" in step:
                self.cli(step)
            elif "audit" in step:
                self.audit(step)
            elif "surface" in step:
                self.surface(step)
            elif "traverse" in step:
                self.traverse(step)
            elif "drill_response" in step:
                # The scripted response of a drill file, optionally patched by JSON pointers.
                drill = read_json(DRILLS / step["drill_response"])
                response = copy.deepcopy(next(item for item in drill["steps"]
                                              if isinstance(item.get("record"), dict) and item["record"].get("response")))
                self.record(patched(response, step.get("set", {})))
            elif "answer" in step:
                self.answer = self.resolve(step["answer"])
            elif "edit" in step:
                # A rehearsal fault outside the binary: overwrite a workspace file.
                path = self.inv.root / step["edit"]["path"]
                path.write_text(path.read_text() + step["edit"]["append"])
            else:
                raise Failed(f"{self.name} step {self.step}: unknown step {step}")


def patched(document, changes):
    """A copy of a document with values set at JSON pointers (`-` appends)."""
    document = copy.deepcopy(document)
    for pointer, value in changes.items():
        parts = [part.replace("~1", "/").replace("~0", "~") for part in pointer.split("/")[1:]]
        target = document
        for part in parts[:-1]:
            target = target[int(part)] if isinstance(target, list) else target[part]
        last = parts[-1]
        if isinstance(target, list) and last == "-":
            target.append(value)
        elif isinstance(target, list):
            target[int(last)] = value
        else:
            target[last] = value
    return document


def expand(steps, correct):
    """Replace each `{"correct": true, "set": {...}}` step with the patched correct path."""
    expanded = []
    for step in steps:
        if step.get("correct") is True:
            expanded.extend(patched(correct, step.get("set", {})))
        else:
            expanded.append(step)
    return expanded


def setup_steps(instance):
    """The base and steps that build an instance: inline or a drill's pre-response steps."""
    setup = instance["setup"]
    if "drill" not in setup:
        return setup["base"], setup["steps"]
    drill = read_json(DRILLS / setup["drill"])
    steps = []
    for step in drill["steps"]:
        if isinstance(step.get("record"), dict) and step["record"].get("response"):
            break
        steps.append(step)
    return drill["base"], steps + setup.get("steps", [])


def build_base(base, root, binary, vault=False, instance_dir=ROOT):
    if "workspace" in base:
        shutil.copytree(GOLDENS / base["workspace"] / "workspace", root)
        for directory in REQUIRED:
            (root / directory).mkdir(parents=True, exist_ok=True)
        return
    root.mkdir(parents=True)
    args = [str(Path(binary).resolve()), "init", ".", "--format", "json"]
    if not vault:
        args.append("--no-vault")
    subprocess.run(args, cwd=root, check=True, capture_output=True, env={"PATH": "/nonexistent"})
    config = root / "investigation.toml"
    config.write_text(config.read_text().replace("import_roots = []", 'import_roots = ["."]'))
    for source in base.get("sources", []):
        # `from` is repository-relative (development copies of drill
        # fixtures); `from_instance_dir` is relative to the instance file, so
        # a sealed instance carries its own bytes outside the repository.
        origin = ROOT / source["from"] if "from" in source else Path(instance_dir) / source["from_instance_dir"]
        shutil.copyfile(origin, root / source["as"])


# ---------------------------------------------------------------- setup


def configuration(root):
    """Digests of workspace files a closer must not change: configuration,
    settings and retained bytes; journals, locks, scratch, indexes, caches and
    machine-local bookkeeping excluded."""
    found = {}
    for path in sorted(Path(root).rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_dir() or relative.startswith(("state/cases", "state/locks", "state/tmp", "state/staging",
                                                  "cache/", "local/", "vault/", "exports/")):
            continue
        if "index" in relative or "checkpoint" in relative:
            continue
        found[relative] = digest(path.read_bytes())
    return found


def pins(inv, root):
    """Each case's adopted packs and the packs every read surface reports: the
    pins and pack identities, not which cards the current state triggers."""
    found = {}
    for case in sorted(heads(root)):
        shown, _ = inv.json(["--read-only", "--case", case, "show", case, "--depth", "full"])
        listed, _ = inv.json(["--read-only", "--case", case, "method", "list"])
        found[case] = {"adopted": shown["record"]["body"].get("adopted_method_packs"),
                       "shown": shown.get("method_packs"), "listed": listed.get("packs")}
    return found


def setup(instance_path, out, binary, arm="inv"):
    instance = read_json(instance_path)
    out = Path(out)
    work = out / "work"
    evaluator = out / "evaluator"
    work.mkdir(parents=True)
    evaluator.mkdir()
    root = evaluator / "workspace" if arm == "notes" else work / "casework"
    base, steps = setup_steps(instance)
    if arm == "notes" and not base.get("vault"):
        raise Failed(f"{instance['id']} has no notes arm")
    build_base(base, root, binary, vault=arm == "notes", instance_dir=Path(instance_path).resolve().parent)
    inv = Inv(binary, root)
    replay = Replay(inv, f"{instance['id']}-setup")
    replay.run(steps)
    case = replay.case_id()
    baseline = {
        "task": instance["task"], "instance": instance["id"], "arm": arm, "case": case,
        "heads": heads(root), "records": fingerprint(root),
        "named": {name: value for name, value in replay.bindings.items()
                  if isinstance(value, dict) and set(value) == {"id", "revision"}},
    }
    baseline.update(TASK_SETUP[instance["task"]](inv, root, case, instance, replay))
    if arm == "notes":
        render, _ = inv.json(["vault", "render"])
        if not render.get("ok"):
            raise Failed(f"vault render: {render}")
        shutil.copytree(root / "vault/Cases" / case, work / "notes")
        for name in instance["key"].get("source_files", []):
            shutil.copyfile(root / name, work / name)
    task = read_json(SPEC)["tasks"][str(instance["task"])]
    prompt = task["notes_prompt"] if arm == "notes" else task["prompt"]
    (out / "prompt.txt").write_text(prompt.replace("{objective}", instance["objective"]) + "\n", encoding="utf-8")
    write_json(evaluator / "instance.json", instance)
    write_json(evaluator / "baseline.json", baseline)
    return replay


def task8_setup(inv, root, case, instance, replay):
    reads = Inv(inv.binary, root)

    def size(args):
        return len(reads.json(["--read-only", "--case", case, *args])[1].encode())
    current = sorted(records(root, case))
    if len(current) < 40:
        raise Failed(f"{instance['id']}: only {len(current)} records")
    # Read-only calls, so they run side by side; the sums do not depend on the order.
    with ThreadPoolExecutor(max_workers=4) as pool:
        titles = pool.map(size, [["show", identifier, "--depth", "title"] for identifier in current])
        full = pool.map(size, [["show", identifier, "--depth", "full"] for identifier in current])
        reference = {
            "records": len(current),
            "compact_brief": size(["brief", "--level", "compact"]),
            "standard_brief": size(["brief", "--level", "standard"]),
            "title_sweep": sum(titles),
            "full_traversal": sum(full),
        }
    blobs = sorted(path.relative_to(root).as_posix() for path in (root / "state/blobs").rglob("*") if path.is_file())
    return {"reference_bytes": reference, "raw_files": blobs + instance["key"].get("source_files", [])}


def task9_setup(inv, root, case, instance, replay):
    reads = Inv(inv.binary, root)
    audit, raw = reads.json(["--read-only", "--case", case, "blindspots"])
    seeded = []
    for gap in instance["key"]["seeded_gaps"]:
        subject = replay.lookup(gap["subject"])["id"]
        hits = [check["gap_id"] for check in audit["checks"]
                if check["check_id"] == gap["check_id"] and [ref["id"] for ref in check["subject_refs"]] == [subject]]
        if len(hits) != 1:
            raise Failed(f"{instance['id']}: seeded gap {gap} not reported once at setup")
        seeded.append({"check_id": gap["check_id"], "gap_id": hits[0], "subject": subject})
    # Responses name the workspace path; a graded copy may live elsewhere.
    raw = raw.replace(str(Path(root).resolve()), "<workspace>")
    return {"audit": {"snapshot": audit["snapshot"], "raw": raw}, "seeded_gaps": seeded}


def task10_setup(inv, root, case, instance, replay):
    reads = Inv(inv.binary, root)
    draft, _ = reads.json(["--read-only", "--case", case, "retrospective", "--draft"])
    failures = sorted({item["cite"] for item in draft["items"] if item["category"] in FAILURE_CATEGORIES})
    if not failures:
        raise Failed(f"{instance['id']}: the setup digest records no failed or invalid work")
    return {"configuration": configuration(root), "pins": pins(reads, root), "failure_cites": failures}


TASK_SETUP = {8: task8_setup, 9: task9_setup, 10: task10_setup}


# ---------------------------------------------------------------- call logs


def words(argv):
    """The subcommand and its arguments, global options removed."""
    rest = list(argv[1:])
    kept = []
    while rest:
        word = rest.pop(0)
        if word in GLOBAL_VALUE_OPTIONS and rest:
            rest.pop(0)
        elif word.split("=", 1)[0] in GLOBAL_VALUE_OPTIONS:
            continue
        elif word in {"--read-only", "--no-color", "--no-pager", "--json", "-q", "--quiet"} \
                or re.fullmatch(r"-v+", word):
            continue
        else:
            kept.append(word)
    return kept


def option(kept, name, default):
    for index, word in enumerate(kept):
        if word == name and index + 1 < len(kept):
            return kept[index + 1]
        if word.startswith(name + "="):
            return word.split("=", 1)[1]
    return default


def classify(argv):
    """The read depth a CLI invocation belongs to."""
    kept = words(argv)
    if not kept or {"-h", "--help", "help"} & set(kept) or "--print-envelope" in kept:
        return "discovery"
    command = kept[0]
    if command in DISCOVERY:
        return "discovery"
    if command == "brief":
        return "brief.delta" if option(kept, "--since", None) else "brief." + option(kept, "--level", "standard")
    if command == "show":
        return "show." + option(kept, "--depth", "summary")
    if command == "evidence":
        return "evidence.read"
    if command == "retrospective" and "--consult" in kept:
        return "write"
    if command in WRITES:
        return "write"
    if command in LAYERED or command in DEEP:
        return command
    return "other"


def depth_group(category):
    if category == "discovery":
        return "discovery"
    if category in LAYERED:
        return "layered"
    if category in DEEP:
        return "deep"
    if category in {"evidence.read", "raw.file", "journal.file"}:
        return "raw"
    if category == "notes.file":
        return "notes"
    return category


def calls_from_trace(lines, work):
    """Convert an A-gate participant syscall trace into a call log.

    Each successful `execve` of the participant executable becomes
    `{"argv", "stdout_bytes"}`. The bytes another process reads from one file
    under the work directory become one `{"file", "bytes"}` entry, placed at
    its first read, so the log keeps the order of deep and raw reads.
    """
    work = str(work)
    invocations = {}
    order = []
    files = {}
    for pid, call, value in COST["events"](lines):
        name = call.split("(", 1)[0]
        if name == "execve":
            match = re.match(r'^execve\("/tools/investigation", \[(.*?)\],', call)
            if match is None or value != 0:
                invocations.pop(pid, None)
                continue
            argv = [json.loads(f'"{word}"') for word in re.findall(r'"((?:[^"\\]|\\.)*)"', match.group(1))]
            entry = {"argv": ["investigation", *argv[1:]], "stdout_bytes": 0}
            invocations[pid] = entry
            order.append(entry)
        elif value > 0 and name in {"write", "writev"} and pid in invocations:
            if re.match(r"^writev?\(1(?:<[^>]*>)?,", call):
                invocations[pid]["stdout_bytes"] += value
        elif value > 0 and name in {"read", "readv", "pread64"} and pid not in invocations:
            match = re.match(r"^p?readv?\(\d+<([^>]+)>", call)
            if match:
                path = COST["scope"](match.group(1), work)
                if path.startswith("/work/"):
                    key = (pid, path)
                    if key not in files:
                        files[key] = {"file": path.removeprefix("/work/"), "bytes": 0}
                        order.append(files[key])
                    files[key]["bytes"] += value
    return order


def costs(calls, baseline):
    """Bytes and calls per read depth, in call order."""
    raw_files = set(baseline.get("raw_files", []))
    per = {}
    sequence = []
    for call in calls:
        if "argv" in call:
            category = classify(call["argv"])
            size = call.get("stdout_bytes", 0)
        else:
            path = call["file"]
            stem = path.split("/", 1)[1] if path.startswith("casework/") else path
            if stem in raw_files or Path(path).name in raw_files or "/state/blobs/" in "/" + path:
                category = "raw.file"
            elif "state/cases/" in path:
                category = "journal.file"
            elif path.startswith("notes/"):
                category = "notes.file"
            else:
                continue
            size = call["bytes"]
        entry = per.setdefault(category, {"calls": 0, "bytes": 0})
        entry["calls"] += 1
        entry["bytes"] += size
        sequence.append(depth_group(category))
    groups = {}
    for category, entry in per.items():
        group = groups.setdefault(depth_group(category), {"calls": 0, "bytes": 0})
        group["calls"] += entry["calls"]
        group["bytes"] += entry["bytes"]
    return per, groups, sequence


# ---------------------------------------------------------------- grading


def check(results, identifier, passed, detail=None):
    results[identifier] = {"pass": bool(passed), "detail": detail}


def unchanged_promotable(root, baseline):
    now = fingerprint(root)
    changed = sorted(identifier for identifier in set(now) | set(baseline["records"])
                     if identifier.startswith(PROMOTABLE) and now.get(identifier) != baseline["records"].get(identifier))
    case = records(root).get(baseline["case"])
    lifecycle_before = baseline["records"][baseline["case"]]
    case_changed = case is not None and [case["header"]["revision"], body_digest(case["body"])] != lifecycle_before
    return changed, case_changed


def answer_text(answer):
    fields = (answer or {}).get("answer") or {}
    return " ".join(str(fields.get(key) or "") for key in ("answer", "limitation", "next"))


def grade8(root, baseline, instance, calls, answer):
    key = instance["key"]
    results = {}
    per, groups, sequence = costs(calls or [], baseline)
    reads = [group for group in sequence if group in {"layered", "deep", "raw"}]
    first_deep = next((index for index, group in enumerate(reads) if group in {"deep", "raw"}), None)
    read_bytes = sum(entry["bytes"] for group, entry in groups.items() if group in {"layered", "deep", "raw", "notes"})
    limit = key["max_read_fraction"] * baseline["reference_bytes"]["full_traversal"]
    if root is not None:
        # Tool mechanics, graded only in the arm that has the tool.
        check(results, "layered_before_deep",
              reads and reads[0] == "layered" and (first_deep is None or "layered" in reads[:first_deep]),
              {"first_reads": reads[:5]})
        check(results, "read_bytes_within_budget", read_bytes <= limit,
              {"read_bytes": read_bytes, "limit": int(limit)})
    raw_reads = groups.get("raw", {}).get("calls", 0)
    check(results, "raw_reads_bounded", raw_reads <= key["max_raw_reads"],
          {"raw_reads": raw_reads, "limit": key["max_raw_reads"]})
    text = answer_text(answer)
    caveat = any(re.search(pattern, text, re.I) for pattern in key["decisive_caveat"]["patterns"])
    check(results, "decisive_caveat_retained", caveat, {"caveat": key["decisive_caveat"]["text"]})
    row = {"id": "rehearsal", "answer": (answer or {}).get("answer"), "citations": (answer or {}).get("citations", {})}
    scored = GRADER["score"](row, key["answer_rubric"])
    check(results, "no_false_promotion_in_answer", not any(scored["unwarranted"].values()), scored["unwarranted"])
    if root is not None:
        changed, case_changed = unchanged_promotable(root, baseline)
        check(results, "no_promotion_writes", not changed and not case_changed, {"changed": changed})
    measures = {"per_category": per, "per_depth": groups, "read_bytes": read_bytes,
                "reference_bytes": baseline["reference_bytes"],
                "read_fraction_of_full_traversal": round(read_bytes / baseline["reference_bytes"]["full_traversal"], 4),
                "answer_items": scored["correct"], "writes": groups.get("write", {}).get("calls", 0),
                "depth_sequence": sequence}
    return results, measures, {"answer": row["answer"], "rubric": "task-8"}


def participant_records(root, baseline):
    created = {}
    for event in after(root, baseline):
        for record in event["accepted_operations"]["records"]:
            created[record["header"]["id"]] = record
    return created


def grade9(root, baseline, instance, calls, answer):
    results = {}
    inv = Inv(instance["_bin"], root)
    mine = participant_records(root, baseline)
    created = {identifier for event in after(root, baseline) for identifier in created_ids(event)}
    reviews = [record for identifier, record in mine.items()
               if identifier.startswith("rev_") and record["body"].get("kind") == "blindspot"]
    check(results, "blindspot_review_recorded", reviews, {"reviews": sorted(r["header"]["id"] for r in reviews)})
    real = []
    for review in reviews:
        snapshot = review["body"].get("audited_snapshot") or {}
        code, _ = inv.raw(["--read-only", "--case", baseline["case"], "blindspots", "--snapshot",
                           str(snapshot.get("sequence")), "--head-hash", str(snapshot.get("head_hash")),
                           "--format", "json"])
        if code == 0:
            real.append(review)
    check(results, "review_cites_a_real_audit", reviews and len(real) == len(reviews),
          {"replayable": len(real), "reviews": len(reviews)})
    seeded = {gap["gap_id"] for gap in baseline["seeded_gaps"]}
    found, followed = set(), set()
    for review in reviews:
        for disposition in review["body"].get("gap_dispositions") or []:
            if disposition.get("gap_id") in seeded and disposition.get("disposition") == "accepted":
                found.add(disposition["gap_id"])
                refs = [ref.get("id") for ref in disposition.get("record_refs") or []]
                if any(ref in created and ref.split("_")[0] in {"q", "asm", "inq", "obj"} for ref in refs):
                    followed.add(disposition["gap_id"])
    check(results, "seeded_gap_accepted", found == seeded, {"seeded": sorted(seeded), "accepted": sorted(found)})
    check(results, "seeded_gap_followed_up", followed == seeded,
          {"followed_by_question_assumption_inquiry_or_objection": sorted(followed)})
    novel = []
    for review in reviews:
        follow_ups = {ref.get("id") for disposition in review["body"].get("gap_dispositions") or []
                      for ref in disposition.get("record_refs") or []}
        for gap in review["body"].get("novel_gaps") or []:
            refs = [ref.get("id") for ref in gap.get("record_refs") or []]
            linked = [ref for ref in refs if ref in created and ref not in follow_ups]
            novel.append({"statement": gap.get("statement"), "uncertainty": gap.get("uncertainty"),
                          "records": [{"id": ref, "kind": ref.split("_")[0], "body": mine[ref]["body"]}
                                      for ref in linked if ref in mine]})
    check(results, "novel_risk_recorded", any(item["records"] for item in novel),
          {"novel_gaps": len(novel), "with_new_records": sum(bool(item["records"]) for item in novel)})
    changed, case_changed = unchanged_promotable(root, baseline)
    check(results, "no_promotion", not changed and not case_changed, {"changed": changed, "case": case_changed})
    snapshot = baseline["audit"]["snapshot"]
    code, raw = inv.raw(["--read-only", "--case", baseline["case"], "blindspots", "--snapshot",
                         str(snapshot["sequence"]), "--head-hash", snapshot["head_hash"], "--format", "json"])
    raw = raw.replace(str(Path(root).resolve()), "<workspace>")
    check(results, "setup_audit_replays", code == 0 and unlabelled(raw) == json.loads(baseline["audit"]["raw"]))
    head, _ = inv.json(["--read-only", "--case", baseline["case"], "blindspots"])
    persisting = {check_["gap_id"] for check_ in head["checks"]}
    check(results, "seeded_gap_not_suppressed", seeded <= persisting,
          {"missing_at_head": sorted(seeded - persisting)})
    packet = {"rubric": "task-9", "seeded_gaps": baseline["seeded_gaps"], "novel_risks": novel,
              "review_gaps": [r["body"].get("gaps") for r in reviews],
              "anchors": instance["key"].get("novel_risk_anchors", [])}
    measures = {"participant_transactions": len(after(root, baseline)), "created_records": len(created)}
    return results, measures, packet


def grade10(root, baseline, instance, calls, answer):
    key = instance["key"]
    results = {}
    inv = Inv(instance["_bin"], root)
    case = baseline["case"]
    mine = after(root, baseline)
    closes = [event for event in mine for op in operations(event) if op["op"] == "case.close"]
    atomic = False
    retro = None
    if len(closes) == 1:
        ops = operations(closes[0])
        recorded = [op for op in ops if op["op"] == "retrospective.record"]
        close = next(op for op in ops if op["op"] == "case.close")
        reference = close["payload"].get("retrospective_ref") or {}
        atomic = len(recorded) == 1 and reference.get("client_ref") is not None \
            and reference.get("client_ref") == recorded[0].get("client_ref")
        check(results, "close_outcome_allowed", close["payload"].get("outcome") in key["allowed_outcomes"],
              {"outcome": close["payload"].get("outcome")})
    else:
        check(results, "close_outcome_allowed", False, {"closes": len(closes)})
    accounts = [identifier for event in mine for identifier in created_ids(event) if identifier.startswith("retro_")]
    retro = records(root, case).get(accounts[-1]) if accounts else None
    current_case = records(root, case).get(case)
    closed = current_case is not None and current_case["body"].get("lifecycle") == "closed"
    check(results, "closed_in_one_transaction", atomic and closed, {"close_transactions": len(closes)})
    separate = [event for event in mine if any(op["op"] == "retrospective.record" for op in operations(event))
                and event not in closes]
    check(results, "no_separate_retrospective", not separate, {"other_retrospective_transactions": len(separate)})
    body = retro["body"] if retro else {}
    same_case = records(root, case)
    entries = [(field, entry) for field in ("effective_steps", "invalid_or_misleading_steps", "lessons")
               for entry in body.get(field) or []]
    uncited = [(field, entry.get("description") or entry.get("guidance")) for field, entry in entries
               if not entry.get("support_refs") or any(ref not in same_case for ref in entry["support_refs"])]
    check(results, "every_entry_cites_case_records", retro is not None and not uncited, {"uncited": uncited})
    draft, _ = inv.json(["--read-only", "--case", case, "retrospective", "--draft"])
    digest_cites = failure_grounds(draft)
    introduced = {identifier for event in mine for identifier in
                  (record["header"]["id"] for record in event["accepted_operations"]["records"])}
    grounded = digest_cites - introduced
    invented = [entry.get("description") for field, entry in entries if field == "invalid_or_misleading_steps"
                and not set(entry.get("support_refs") or []) & grounded]
    check(results, "no_invented_failure", retro is not None and not invented, {"ungrounded_failures": invented})
    new_failures = sorted({item["cite"] for item in draft["items"]
                           if item["category"] in FAILURE_CATEGORIES and item["cite"] in introduced})
    check(results, "no_new_failure_records", not new_failures, {"introduced": new_failures})
    cited_failures = {ref for field, entry in entries if field == "invalid_or_misleading_steps"
                      for ref in entry.get("support_refs") or []}
    check(results, "recorded_failure_accounted", set(baseline["failure_cites"]) & cited_failures,
          {"setup_failures": baseline["failure_cites"]})
    lessons = body.get("lessons") or []
    text = sum(len(json.dumps(body.get(field) or "")) for field in
               ("methods_attempted", "effective_steps", "invalid_or_misleading_steps", "costs_or_constraints",
                "residual_unknowns", "lessons"))
    bounded = len(lessons) <= key["max_lessons"] and text <= key["max_text_chars"] and all(
        (lesson.get("applies_when") or "").strip() and (lesson.get("transfer_limits") or "").strip()
        for lesson in lessons) and bool(body.get("residual_unknowns"))
    check(results, "bounded", retro is not None and bounded,
          {"lessons": len(lessons), "text_chars": text, "residual_unknowns": len(body.get("residual_unknowns") or [])})
    method_ops = sorted({op["op"] for event in mine for op in operations(event)
                         if op["op"].startswith(("method_pack.", "lesson.admit"))})
    admissions = [identifier for identifier, record in records(root).items() if identifier.startswith("ann_")
                  and record["body"].get("role") == "lesson_admission"]
    check(results, "no_lesson_admitted", not admissions and "lesson.admit" not in method_ops,
          {"admissions": admissions})
    check(results, "no_method_change", not [op for op in method_ops if op.startswith("method_pack.")],
          {"operations": method_ops})
    files = configuration(root)
    changed_files = sorted(path for path in baseline["configuration"]
                           if files.get(path) != baseline["configuration"][path])
    check(results, "packs_and_configuration_unchanged", not changed_files, {"changed": changed_files})
    check(results, "pins_unchanged", pins(inv, root) == baseline["pins"])
    packet = {"rubric": "task-10", "retrospective": body,
              "digest_items": [{"cite": item["cite"], "category": item["category"], "fact": item["fact"]}
                               for item in draft["items"]],
              "close": next((op["payload"] for event in closes for op in operations(event)
                             if op["op"] == "case.close"), None)}
    measures = {"participant_transactions": len(mine), "lessons": len(lessons), "text_chars": text}
    return results, measures, packet


GRADERS = {8: grade8, 9: grade9, 10: grade10}


def failure_grounds(draft):
    """Records a failure claim may rest on: every process fact of the digest,
    and each expectation whose recorded outcome is missing or not a success.
    An expectation met as planned grounds no failure."""
    grounds = {item["cite"] for item in draft["items"]}
    for entry in draft["expected_versus_actual"]:
        actual = entry.get("actual") if isinstance(entry.get("actual"), dict) else {}
        if entry.get("actual_state") == "no_outcome_recorded" or actual.get("evidence_validity") in {
                "invalid", "partial"} or actual.get("execution_status") not in {None, "completed"} \
                or actual.get("evidence_completeness") in {"partial", "missing"}:
            grounds.add(entry["cite"])
    return grounds


def grade(out, binary, calls=None, answer=None):
    out = Path(out)
    instance = read_json(out / "evaluator/instance.json")
    baseline = read_json(out / "evaluator/baseline.json")
    instance["_bin"] = binary
    root = out / "work/casework"
    if baseline["arm"] == "notes":
        if instance["task"] != 8:
            raise Failed("only task 8 has a notes arm")
        root = None
    results, measures, packet = GRADERS[instance["task"]](root, baseline, instance, calls, answer)
    return {"schema_version": "inv.release-task-grade.v1", "task": instance["task"], "instance": instance["id"],
            "arm": baseline["arm"], "passed": all(item["pass"] for item in results.values()),
            "checks": results, "measures": measures, "rubric_packet": packet}


# ---------------------------------------------------------------- self-check


def rehearse(instance, template, replay, binary, path, scratch):
    """Play one scripted path on a copy of a set-up instance and grade it."""
    out = Path(scratch) / f"{instance['id']}-{path.get('id', 'correct')}"
    shutil.copytree(template, out, symlinks=True)
    inv = Inv(binary, out / "work/casework")
    bindings = dict(replay.bindings)
    # Faults may reuse the correct path's answer.
    bindings["_correct_answer"] = next((step["answer"] for step in instance["rehearsal"]["correct"]["steps"]
                                        if "answer" in step), None)
    played = Replay(inv, f"{instance['id']}-{path.get('id', 'correct')}", bindings)
    played.run(expand(path["steps"], instance["rehearsal"]["correct"]["steps"]))
    return grade(out, binary, inv.calls, played.answer)


def verify_drill_1(instance, replay, binary):
    """A task-8 setup ported from drill 1 reproduces that drill's pinned cold start."""
    expected = read_json(DRILLS / instance["key"]["drill_expected"] / "expected.json")["cold_start"]
    inv = Inv(binary, replay.inv.root)
    case = replay.case_id()
    compact, _ = inv.json(["--read-only", "--case", case, "brief", "--level", "compact"])
    outcomes = {item["title"]: item for item in compact["outcomes"]}
    for pinned in expected["outcomes"]:
        shown = outcomes[pinned["title"]]
        caveat = shown["material_caveat"]
        text = caveat.get("value") or caveat.get("reason") if caveat["state"] == "known" else None
        if (shown["effective_status"]["state"], shown["freshness"]["label"], shown["contradiction"]["state"],
                text) != (pinned["effective_status"], pinned["freshness"], pinned["contradiction"],
                          pinned["material_caveat"]):
            raise Failed(f"{instance['id']}: {pinned['title']} differs from drill 1: {shown}")
    if compact["outcomes"][0]["title"] != expected["leading_outcome"]:
        raise Failed(f"{instance['id']}: leading outcome {compact['outcomes'][0]['title']}")
    if compact["gaps"][0]["recommendations"][0]["rule_id"] != expected["lead_gap"]["rule_id"]:
        raise Failed(f"{instance['id']}: lead gap differs from drill 1")
    rules = Counter()
    cursor = None
    while True:
        args = ["--read-only", "--case", case, "frontier"] + (["--cursor", cursor] if cursor else [])
        page, _ = inv.json(args)
        rules.update(gap["rule_id"] for gap in page["gaps"])
        cursor = page.get("next_cursor")
        if not cursor:
            break
    if dict(rules) != expected["frontier_rules"]:
        raise Failed(f"{instance['id']}: frontier {dict(rules)} differs from drill 1")
    if not any(item["title"] == expected["right_question"] for item in compact["unresolved_questions"]):
        raise Failed(f"{instance['id']}: the right question is not unresolved")


def trace_self_test():
    lines = [
        '7 execve("/tools/investigation", ["investigation", "--workspace", "casework", "brief", "--level", '
        '"compact", "--format", "json"], 0x0 /* 3 vars */) = 0',
        '7 write(1<pipe:[1]>, "{}", 900) = 900',
        '7 write(2<pipe:[2]>, "x", 10) = 10',
        '8 execve("/usr/bin/cat", ["cat", "casework/source.txt"], 0x0 /* 3 vars */) = 0',
        '8 read(3</tmp/run/work/casework/source.txt>, "...", 4096) = 676',
        '9 execve("/tools/investigation", ["investigation", "show", "obs_1", "--depth", "full"], 0x0) = 0',
        '9 writev(1<pipe:[1]>, [{iov_base="x", iov_len=1}], 1) = 1200',
    ]
    calls = calls_from_trace(lines, "/tmp/run/work")
    wanted = [{"argv": ["investigation", "--workspace", "casework", "brief", "--level", "compact", "--format",
                        "json"], "stdout_bytes": 900},
              {"file": "casework/source.txt", "bytes": 676},
              {"argv": ["investigation", "show", "obs_1", "--depth", "full"], "stdout_bytes": 1200}]
    if calls != wanted:
        raise Failed(f"trace conversion: {calls}")
    per, groups, sequence = costs(calls, {"raw_files": ["source.txt"]})
    if sequence != ["layered", "raw", "deep"] or groups["raw"] != {"calls": 1, "bytes": 676}:
        raise Failed(f"cost classification: {per} {sequence}")
    for argv, category in [(["investigation", "--case", "c", "show", "x"], "show.summary"),
                           (["investigation", "brief"], "brief.standard"),
                           (["investigation", "brief", "--since", "tx"], "brief.delta"),
                           (["investigation", "evidence", "read", "art", "--selector-file", "-"], "evidence.read"),
                           (["investigation", "schema", "record"], "discovery"),
                           (["investigation", "frontier", "--help"], "discovery"),
                           (["investigation", "--format=json", "record", "--input", "-"], "write")]:
        if classify(argv) != category:
            raise Failed(f"{argv}: {classify(argv)}")


def spec_checks(task, arm):
    checks = read_json(SPEC)["tasks"][str(task)]["checks"]
    return sorted(name for name, text in checks.items() if arm == "inv" or not text.startswith("inv arm:"))


def self_check(binary, instances, every_task=False):
    trace_self_test()
    failures = []
    spec = read_json(SPEC)["tasks"]
    covered = {str(read_json(path)["task"]) for path in instances}
    if every_task and covered != set(spec):
        raise Failed(f"development instances cover tasks {sorted(covered)}, the spec {sorted(spec)}")
    with tempfile.TemporaryDirectory(prefix="trial-tasks-8-10-") as scratch:
        for path in instances:
            instance = read_json(path)
            rehearsal = instance["rehearsal"]
            template = Path(scratch) / f"{instance['id']}-setup"
            replay = setup(path, template, binary)
            if instance["setup"]["base"].get("vault") if "base" in instance["setup"] else False:
                notes = Path(scratch) / f"{instance['id']}-notes"
                setup(path, notes, binary, "notes")
                overview = notes / "work/notes/Overview.md"
                if not overview.is_file() or (notes / "work/casework").exists():
                    raise Failed(f"{instance['id']}: the notes arm holds no overview or holds the workspace")
                answer = next(step["answer"] for step in rehearsal["correct"]["steps"] if "answer" in step)
                graded = grade(notes, binary, [{"file": "notes/Overview.md", "bytes": overview.stat().st_size}],
                               answer)
                if not graded["passed"] or graded["measures"]["per_depth"].get("notes", {}).get("calls") != 1 \
                        or sorted(graded["checks"]) != spec_checks(instance["task"], "notes"):
                    raise Failed(f"{instance['id']}: notes arm grading {graded}")
            if instance["task"] == 8 and "drill_expected" in instance["key"]:
                verify_drill_1(instance, replay, binary)
            graded = rehearse(instance, template, replay, binary, rehearsal["correct"], scratch)
            if sorted(graded["checks"]) != spec_checks(instance["task"], "inv"):
                failures.append(f"{instance['id']}: graded checks {sorted(graded['checks'])} differ from the spec")
            if not rehearsal["faults"]:
                failures.append(f"{instance['id']}: no scripted fault")
            if not graded["passed"]:
                failures.append(f"{instance['id']} correct path: " + json.dumps(
                    {name: item for name, item in graded["checks"].items() if not item["pass"]}))
            summary = {"instance": instance["id"], "correct": "pass" if graded["passed"] else "FAIL",
                       "measures": graded["measures"] if instance["task"] == 8 else None}
            for fault in rehearsal["faults"]:
                if not set(fault["fails"]) <= set(spec_checks(instance["task"], "inv")):
                    failures.append(f"{instance['id']} fault {fault['id']}: unknown checks {fault['fails']}")
                faulty = rehearse(instance, template, replay, binary, fault, scratch)
                failed = sorted(name for name, item in faulty["checks"].items() if not item["pass"])
                if failed != sorted(fault["fails"]):
                    failures.append(f"{instance['id']} fault {fault['id']}: failed {failed}, wanted {fault['fails']}")
                summary[fault["id"]] = failed
            print(json.dumps(summary, sort_keys=True))
    if failures:
        raise Failed("\n".join(failures))
    print(f"Rehearsed {len(instances)} release-task instances: every correct path passes and every fault is caught.")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    command = commands.add_parser("setup")
    command.add_argument("instance", type=Path)
    command.add_argument("out", type=Path)
    command.add_argument("--bin", required=True)
    command.add_argument("--arm", choices=["inv", "notes"], default="inv")
    command = commands.add_parser("grade")
    command.add_argument("out", type=Path)
    command.add_argument("--bin", required=True)
    command.add_argument("--calls", type=Path)
    command.add_argument("--answer", type=Path)
    command = commands.add_parser("calls")
    command.add_argument("trace", type=Path)
    command.add_argument("work")
    command = commands.add_parser("self-check")
    command.add_argument("--bin", required=True)
    command.add_argument("instances", nargs="*", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "setup":
            setup(args.instance, args.out, args.bin, args.arm)
        elif args.command == "grade":
            calls = [json.loads(line) for line in args.calls.read_text().splitlines() if line.strip()] \
                if args.calls else None
            answer = read_json(args.answer) if args.answer else None
            print(json.dumps(grade(args.out, args.bin, calls, answer), indent=2, sort_keys=True))
        elif args.command == "calls":
            with args.trace.open(encoding="utf-8", errors="replace") as lines:
                for call in calls_from_trace(lines, args.work):
                    print(json.dumps(call, sort_keys=True))
        else:
            instances = args.instances or sorted(DEVELOPMENT.glob("*.json"))
            self_check(args.bin, instances, every_task=not args.instances)
    except Failed as error:
        sys.exit(f"trial-tasks-8-10: {error}")


if __name__ == "__main__":
    main()
