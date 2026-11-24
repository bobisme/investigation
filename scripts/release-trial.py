#!/usr/bin/env python3
"""Release-trial harness (W14.05, plan §20.5): plan, run, grade and freeze the paired trial.

  plan      freeze the binary, inputs, budgets, prompts, backend and a seeded, counterbalanced schedule
  run       run planned assignments; every run writes transcripts, an answer, workspace snapshots,
            a call log and a cost record
  grade     blind and grade answers, grade task mechanics, reconcile, then freeze every record
  verify    check that a frozen trial is unchanged
  dry-run   plan, run and grade the development dry-run set with the offline stub participant
  self-test unit checks plus a full dry run (crates/investigation-cli/tests/release_trial.rs)

Task types: `family` (base scenarios 1-4 in four domains, original and twin), `lookup`
(skip-the-tool), and `task-5` to `task-10`. Arms: `notes` (Markdown and ordinary tools) and `inv`
(the same plus the frozen binary). The record format is in training/release-trial/protocol.md and
training/release-trial/run-record.schema.json.

The harness never runs a model on its own. A backend is chosen at plan time: `stub` (offline,
deterministic, dry runs only), `command` (an operator-supplied adapter that speaks the JSON
protocol in the protocol document) or `local` (the A-gate pilot's local endpoint).
"""

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import copy
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import re
import runpy
import shlex
import shutil
import socketserver
import stat
import subprocess
import sys
import tempfile
import threading
import time
import types
import uuid


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
REGISTRY = FIXTURES / "family-registry.json"
DRY_RUN = ROOT / "training/release-trial/dry-run"
SEALED_MANIFEST = ROOT / "training/release/families/manifest.json"
PREREGISTRATION = ROOT / "docs/release-trial.md"
TASK_SPEC_8_10 = ROOT / "training/trial-tasks/tasks-8-10/tasks-8-10.json"
RUNNER = runpy.run_path(str(ROOT / "scripts/run-a-gate.py"))
GRADER = runpy.run_path(str(ROOT / "scripts/grade-a-gate.py"))
COST = runpy.run_path(str(ROOT / "scripts/a-gate-cost.py"))
FIXTURE = runpy.run_path(str(ROOT / "scripts/check-fixture-families.py"))
TT = runpy.run_path(str(ROOT / "scripts/trial-tasks.py"))
TT810 = runpy.run_path(str(ROOT / "scripts/trial-tasks-8-10.py"))

PLAN_SCHEMA = "inv.release-trial-plan.v1"
RUN_SCHEMA = "inv.release-trial-run.v1"
FROZEN_SCHEMA = "inv.release-trial-frozen.v1"
TASK_TYPES = ("family", "lookup", "task-5", "task-6", "task-7", "task-8", "task-9", "task-10")
ARMS = ("notes", "inv")
# Conditions outside the primary comparison. Each adds single-arm `inv` assignments flagged
# primary: false. W13.06 (MCP) adds its profiles here once that frontend exists.
SECONDARY = {"docs": "The investigator and successor prompts also carry the binary's own agent instructions "
                     "(`investigation docs agents`), as a host loads AGENTS.md (W17.03 docs arm)."}
DOCS_CONTEXT = "\n\nThe repository's AGENTS.md contains these instructions:\n\n{page}"
BUDGETS = copy.deepcopy(RUNNER["BUDGETS"])
ROLE_BUDGET = {"investigator": "investigator", "successor": "successor", "reviewer": "investigator",
               "closer": "investigator"}
INFERENCE = dict(RUNNER["INFERENCE"])
SYSTEM = RUNNER["SYSTEM"]
INVESTIGATE = RUNNER["INVESTIGATE"]
RESUME = RUNNER["RESUME"]
# Tasks 5-7 say "then give your answer"; both arms get the same four-field format.
ANSWER_FORMAT = """Use submit_answer with four nonempty fields:
  "answer": the current answer or choice, its disposition, and any change since review,
  "citation": the decisive source filename and exact line, selector, or distinctive quotation,
  "limitation": the strongest remaining limitation,
  "next": the next observation and what result would change your answer or choice.
Use a source citation that an independent reader can find in the original bytes."""
SHELL_TOOL = RUNNER["TOOLS"][0]
SUBMIT = RUNNER["SUBMIT"]
SHELL_BYTES = RUNNER["SHELL_BYTES"]
SHELL_SECONDS = 12
DOCUMENTS = ("source.txt", "update.txt")
# Pilot sandbox limits (training/a-gate/protocol.md), the same for both arms.
FSIZE = "--fsize=2097152"
NOFILE = "--nofile=128"
COST_FIELDS = (
    "wall_ms", "model_turns", "input_tokens", "output_tokens", "cached_input_tokens_reported",
    "shell_calls", "shell_output_bytes", "truncated_shell_calls", "cli_calls", "discovery_calls",
    "discovery_bytes", "onboard_calls", "evidence_bytes", "note_read_bytes", "note_write_bytes",
    "preparation_calls", "first_receipt_call", "first_onboard_call", "first_note_call",
    "calls_from_onboard_to_receipt", "read_depth", "read_categories", "metering",
)
SUMMED = ("wall_ms", "model_turns", "input_tokens", "output_tokens", "cached_input_tokens_reported",
          "shell_calls", "shell_output_bytes", "truncated_shell_calls", "cli_calls", "discovery_calls",
          "discovery_bytes", "onboard_calls", "evidence_bytes", "note_read_bytes", "note_write_bytes",
          "preparation_calls")
TRACE_FIELDS = ("discovery_calls", "onboard_calls", "discovery_bytes", "evidence_bytes", "note_read_bytes",
                "note_write_bytes", "preparation_calls")


class HarnessError(Exception):
    pass


# --- Small helpers -------------------------------------------------------------------------------


def now():
    return datetime.now(timezone.utc).isoformat()


def sha_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha_file(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write_new(path, value):
    """Write once: a record that already exists is never replaced."""
    with Path(path).open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2, ensure_ascii=False, sort_keys=True)
        output.write("\n")


def relative(path):
    path = Path(path).resolve()
    return path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else str(path)


def version(argv):
    try:
        return subprocess.run(argv, capture_output=True, text=True, check=False).stdout.splitlines()[0].strip()
    except (OSError, IndexError):
        return None


def tree_digest(root):
    return RUNNER["tree_digest"](Path(root))


def git_head():
    try:
        return subprocess.check_output(["git", "-C", ROOT, "rev-parse", "HEAD"], text=True,
                                       stderr=subprocess.DEVNULL).strip()
    except (OSError, subprocess.CalledProcessError):
        return None


# --- Case set: what the trial runs and where its sources and keys are ---------------------------


class CaseSet:
    """The registered families, lookups and trial tasks of one split.

    `development` reads the dry-run manifest (training/release-trial/dry-run), sources from
    fixtures/ and keys from the dry-run keys directory. `sealed` reads the sealed manifest
    (training/release/families) and an opened bundle outside the repository: objectives and sources
    from `<opened>/sealed/`, keys from `<opened>/evaluator/`, each checked against its registered
    digest when used. Keys are only ever read by `grade`.
    """

    def __init__(self, split, opened=None, trial_tasks=None):
        self.split = split
        registry = read_json(REGISTRY)
        if split == "development":
            self.manifest_path = DRY_RUN / "manifest.json"
            self.source_root = FIXTURES
            self.key_root = DRY_RUN / "keys"
            FIXTURE["validate"](registry, FIXTURES)
        else:
            if opened is None:
                raise HarnessError("the sealed split needs --opened, the bundle opened outside the repository")
            opened = Path(opened).resolve()
            if opened.is_relative_to(ROOT):
                raise HarnessError("the opened bundle must be outside the repository")
            self.manifest_path = SEALED_MANIFEST
            self.source_root = opened
            self.key_root = opened / "evaluator"
            FIXTURE["validate"](registry, FIXTURES, sealed_root=opened)
        self.manifest = read_json(self.manifest_path)
        self.families = {family["id"]: family for family in registry["families"]}
        entries = list(self.manifest["families"]) + list(self.manifest.get("lookup_tasks", {}).get("tasks", []))
        self.entries = {entry["id"]: entry for entry in entries}
        for family_id in self.entries:
            if self.families[family_id]["split"] != split:
                raise HarnessError(f"{family_id} is not registered in the {split} split")
        if trial_tasks is None and split == "development":
            trial_tasks = self.manifest.get("trial_tasks", {})
        self.trial_tasks = {str(number): [str(path) for path in paths]
                            for number, paths in (trial_tasks or {}).items()}

    def variant(self, family, variant):
        return next(item for item in self.families[family]["variants"] if item["id"] == variant)

    def objective(self, family, variant):
        entry = self.variant(family, variant)
        if "objective" in entry:
            return entry["objective"]
        path = self.source_root / entry["directory"] / "objective.txt"
        data = path.read_bytes()
        if sha_bytes(data) != entry["objective_sha256"]:
            raise HarnessError(f"objective digest mismatch: {family}/{variant}")
        return data.decode("utf-8").strip()

    def document(self, family, variant, name):
        entry = self.variant(family, variant)
        path = self.source_root / entry["directory"] / name
        return path.read_bytes() if path.is_file() else None

    def key(self, family):
        entry = self.entries[family]
        path = self.key_root / entry["key"]
        if sha_file(path) != entry["key_sha256"]:
            raise HarnessError(f"key digest mismatch: {family}")
        return read_json(path)

    def key_digests(self):
        """Every key and rubric this split could hand to a grader, by digest only."""
        digests = {entry["key_sha256"] for entry in self.entries.values()}
        for path in (ROOT / "training/trial-tasks/keys").glob("*.json"):
            digests.add(sha_file(path))
        for paths in self.trial_tasks.values():
            for path in paths:
                file = resolve_input(path)
                digests.add(sha_file(file))
                if file.suffix == ".json":
                    task = read_json(file)
                    if isinstance(task.get("key"), str):
                        digests.add(sha_file(resolve_input(task["key"])))
        return digests

    def forbidden_paths(self):
        """Host paths that must not be visible inside a participant sandbox."""
        paths = {ROOT, self.key_root, self.manifest_path.parent, ROOT / "training", Path.home()}
        if self.split == "sealed":
            paths.add(self.source_root)
        for task_paths in self.trial_tasks.values():
            for path in task_paths:
                paths.add(resolve_input(path).parent)
        return sorted(str(path) for path in paths)


def resolve_input(path):
    path = Path(path)
    return path if path.is_absolute() else ROOT / path


# --- Units and the counterbalanced schedule -------------------------------------------------------


def units(case_set, secondary=()):
    """The things to run, before models and arm order. Each unit lists the arms it has."""
    result = []
    for entry in case_set.manifest["families"]:
        family = case_set.families[entry["id"]]
        for variant in family["variants"]:
            kind = "original" if variant["id"] == "original" else entry["twin"]
            result.append({"task_type": "family", "task": None, "family": family["id"], "variant": variant["id"],
                           "domain": family["domain"], "twin": kind, "stage": None, "condition": None,
                           "arms": list(ARMS), "primary": True})
            for condition in secondary:
                result.append(dict(result[-1], condition=condition, arms=["inv"], primary=False))
    for entry in case_set.manifest.get("lookup_tasks", {}).get("tasks", []):
        result.append({"task_type": "lookup", "task": None, "family": entry["id"], "variant": "lookup",
                       "domain": "lookup", "twin": None, "stage": None, "condition": None,
                       "arms": list(ARMS), "primary": False, "theme": entry.get("theme")})
    for number in ("5", "6", "7"):
        for path in case_set.trial_tasks.get(number, []):
            task = TT["load_task"](str(resolve_input(path)))
            base = {"task_type": f"task-{number}", "task": path, "family": task["family"],
                    "domain": task["domain"], "twin": None, "primary": False}
            if "stages" not in task:
                result.append(dict(base, variant=task["phase"]["variant"], stage=None, condition=None,
                                   arms=sorted(task["arms"], key=ARMS.index)))
                continue
            for stage, spec in task["stages"].items():
                arms = sorted(spec["arms"], key=ARMS.index)
                conditions = list(task.get("conditions", {})) if stage != "stage-1" else []
                if not conditions:
                    result.append(dict(base, variant=spec["variant"], stage=stage, condition=None, arms=arms))
                    continue
                # The first condition is the paired comparison; the others are tool-arm conditions.
                for index, condition in enumerate(conditions):
                    result.append(dict(base, variant=spec["variant"], stage=stage, condition=condition,
                                       arms=arms if index == 0 else ["inv"]))
    spec = read_json(TASK_SPEC_8_10)["tasks"]
    for number in ("8", "9", "10"):
        for path in case_set.trial_tasks.get(number, []):
            instance = read_json(resolve_input(path))
            if str(instance["task"]) != number:
                raise HarnessError(f"{path} is task {instance['task']}, listed as task {number}")
            result.append({"task_type": f"task-{number}", "task": path, "family": instance["id"],
                           "variant": instance["id"], "domain": instance["domain"], "twin": None, "stage": None,
                           "condition": None, "arms": sorted(spec[number]["arms"], key=ARMS.index),
                           "primary": False})
    return result


def schedule(unit_list, models, seed):
    """Seeded blocks: one per unit and model, with a counterbalanced arm order.

    Families: within each model and domain, families are shuffled and alternate which arm goes
    first; a family's twin takes the opposite order to its original, and the second model takes
    the opposite order to the first, so each family is balanced over its four paired blocks. Other
    paired units alternate within task type and model. Each block is one variant in fresh sessions:
    no session ever sees both variants of a family. Blocks are then shuffled within each model, and
    models run one after another (pilot practice on a shared inference service)."""
    generator = random.Random(seed)
    blocks = []
    for model_index, model in enumerate(models):
        strata = {}
        for unit in unit_list:
            key = ("family", unit["domain"], unit["condition"]) if unit["task_type"] == "family" \
                else (unit["task_type"], unit["stage"], unit["condition"])
            strata.setdefault(key, []).append(unit)
        model_blocks = []
        for key in sorted(strata, key=lambda value: [str(part) for part in value]):
            members = strata[key]
            offset = generator.randrange(2)
            if key[0] == "family":
                by_family = {}
                for unit in members:
                    by_family.setdefault(unit["family"], []).append(unit)
                names = sorted(by_family)
                generator.shuffle(names)
                for index, name in enumerate(names):
                    for unit in by_family[name]:
                        flip = (index + offset + model_index + (unit["twin"] != "original")) % 2
                        model_blocks.append(block(unit, model, flip, generator))
            else:
                ordered = sorted(members, key=lambda unit: (unit["family"], unit["variant"]))
                generator.shuffle(ordered)
                for index, unit in enumerate(ordered):
                    model_blocks.append(block(unit, model, (index + offset + model_index) % 2, generator))
        generator.shuffle(model_blocks)
        blocks.extend(model_blocks)
    assignments = []
    for block_index, item in enumerate(blocks, 1):
        item["block"] = f"b{block_index:04d}"
        for position, arm in enumerate(item.pop("order"), 1):
            assignments.append(dict(item, arm=arm, order=position))
    for sequence, assignment in enumerate(assignments, 1):
        assignment["sequence"] = sequence
        assignment["id"] = f"r{sequence:04d}"
    return assignments


def block(unit, model, flip, generator):
    arms = list(unit["arms"])
    if len(arms) == 2 and flip:
        arms.reverse()
    item = {key: value for key, value in unit.items() if key != "arms"}
    item.update(model=model, order=arms, block_seed=generator.randrange(1, 2**31))
    return item


# --- Backends ------------------------------------------------------------------------------------


class Stub:
    """An offline, deterministic participant for dry runs. It plays the same generic script in
    both arms: list the directory, read what is there, use the CLI if one is on PATH (else notes),
    honour the review checkpoint and end marker, then submit a labelled placeholder answer. It
    reads only what its own session's messages contain."""

    kind = "stub"

    def describe(self, models):
        code = sha_bytes(Path(__file__).read_bytes())
        return {"provider": "stub", "version": code[:16], "models": {model: f"stub:{model}" for model in models},
                "metering": "deterministic estimate: serialized characters / 4"}

    def chat(self, model, messages, tools, options, session_id, timeout):
        names = {tool["function"]["name"] for tool in tools}
        prompt = messages[1]["content"]
        results = [message["content"] for message in messages if message.get("role") == "tool"]
        step = sum(message.get("role") == "assistant" for message in messages)
        listing = json.loads(results[0]).get("output", "") if results else None
        commands = self.plan(prompt, listing)
        if step < len(commands) and "shell" in names:
            reply = {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "shell", "arguments": {"command": commands[step]}}}]}
        elif "submit_answer" in names:
            reply = {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "submit_answer", "arguments": self.answer(results)}}]}
        else:
            reply = {"role": "assistant", "content": "Finished."}
        return {"message": reply, "prompt_eval_count": len(json.dumps(messages)) // 4 + 1,
                "eval_count": len(json.dumps(reply)) // 4 + 1, "done_reason": "stop"}

    @staticmethod
    def plan(prompt, listing):
        commands = ["ls -la; command -v investigation || echo 'no investigation command'"]
        if listing is None:
            return commands
        cli = "no investigation command" not in listing and re.search(r"/investigation\s*$", listing, re.M)
        writes = any(word in prompt.lower() for word in ("durable", "record", "update", "review.ready"))
        if cli:
            commands.append("investigation --help | head -n 12")
        if re.search(r"\bsource\.txt\b", listing):
            commands.append("head -n 12 source.txt")
        in_case = "cd casework 2>/dev/null; "
        if cli and re.search(r"\b(?:casework|investigation\.toml)\b", listing):
            commands.append(in_case + "investigation brief 2>&1 | head -n 30")
        elif cli and writes:
            commands.append("investigation init ./casework --no-vault --import-root . --actor agent:dry-run >/dev/null 2>&1; "
                            "cd casework && investigation start 'Dry-run case' --attach ../source.txt 2>&1 | tail -n 3")
        elif not cli:
            if re.search(r"\bNOTES\.md\b", listing):
                commands.append("head -n 40 NOTES.md")
            if re.search(r"\bnotes\b", listing):
                commands.append("ls notes | head -n 20")
            if writes:
                commands.append("printf '%s\\n' '## Dry-run note' 'Read the sources; no analysis performed.' >> NOTES.md")
        if "review.ready" in prompt:
            commands.append("touch review.ready")
            commands.append("ls; head -n 12 update.txt 2>/dev/null || true")
            commands.append(in_case + "investigation brief 2>&1 | head -n 10" if cli else
                            "printf '%s\\n' 'Read update.txt after review.' >> NOTES.md")
        marker = next((name for name in re.findall(r"\b([a-z]+\.ready)\b", prompt) if name != "review.ready"), None)
        if marker:
            commands.append(f"touch {marker}")
        return commands

    @staticmethod
    def answer(results):
        return {"answer": f"Dry-run placeholder answer after {len(results)} shell calls; no analysis was performed.",
                "citation": "source.txt line 1",
                "limitation": "A scripted dry-run participant produced this answer.",
                "next": "Not applicable to a dry run."}


class CommandBackend:
    """An operator-supplied adapter: one process per model turn, JSON request on stdin, JSON
    response on stdout (see the protocol document). The request carries the whole conversation,
    so an adapter needs no state and cannot carry one session into another."""

    kind = "command"

    def __init__(self, argv):
        self.argv = list(argv)

    def describe(self, models):
        reply = self.call({"kind": "describe", "models": list(models)}, 120)
        if set(reply.get("models", {})) != set(models):
            raise HarnessError("the adapter must describe every model")
        executable = shutil.which(self.argv[0]) or self.argv[0]
        reply["adapter"] = {"argv": self.argv, "sha256": sha_file(executable) if Path(executable).is_file() else None}
        return reply

    def call(self, request, timeout):
        done = subprocess.run(self.argv, input=json.dumps(request), capture_output=True, text=True,
                              timeout=timeout, check=False)
        if done.returncode != 0:
            raise OSError(f"adapter exit {done.returncode}: {done.stderr[-2000:]}")
        return json.loads(done.stdout)

    def chat(self, model, messages, tools, options, session_id, timeout):
        request = {"kind": "chat", "model": model, "session_id": session_id, "messages": messages,
                   "tools": tools, "options": options, "deadline_seconds": timeout}
        try:
            return self.call(request, timeout)
        except subprocess.TimeoutExpired:
            raise TimeoutError from None


# The smallest adapter that follows the protocol. It names no host: a real adapter translates
# the request to its host's API, keeps no state between calls and reports metered tokens.
ADAPTER_EXAMPLE = '''import json, sys
request = json.load(sys.stdin)
if request["kind"] == "describe":
    reply = {"provider": "example", "version": "1", "metering": "example counts",
             "models": {model: "example:" + model for model in request["models"]}}
else:
    fields = {name: "example" for name in ("answer", "citation", "limitation", "next")}
    reply = {"message": {"role": "assistant", "content": "", "tool_calls": [
                 {"function": {"name": "submit_answer", "arguments": fields}}]},
             "prompt_eval_count": len(json.dumps(request["messages"])) // 4, "eval_count": 20,
             "prompt_eval_cached_count": 0, "done_reason": "stop"}
json.dump(reply, sys.stdout)
'''


class LocalBackend:
    """The A-gate pilot's local inference endpoint (scripts/run-a-gate.py)."""

    kind = "local"

    def describe(self, models):
        installed = {model["name"]: model["digest"] for model in RUNNER["request"]("/api/tags")["models"]}
        return {"provider": "local endpoint (A-gate pilot runner)",
                "version": RUNNER["request"]("/api/version")["version"],
                "models": {name: installed[name] for name in models},
                "metering": "reported prompt evaluation plus cached tokens when the server reports them"}

    def chat(self, model, messages, tools, options, session_id, timeout):
        return RUNNER["request"]("/api/chat", {"model": model, "messages": messages, "tools": tools,
                                              "stream": False, "think": True, "options": options}, timeout=timeout)


def make_backend(config, override_command=None):
    if config["kind"] == "stub":
        return Stub()
    if config["kind"] == "command":
        argv = shlex.split(override_command) if override_command else config["argv"]
        backend = CommandBackend(argv)
        executable = shutil.which(argv[0]) or argv[0]
        if config.get("adapter_sha256") and Path(executable).is_file() and sha_file(executable) != config["adapter_sha256"]:
            raise HarnessError("the backend adapter changed since the plan was frozen")
        return backend
    if config["kind"] == "local":
        return LocalBackend()
    raise HarnessError(f"unknown backend {config['kind']}")


# --- Sandbox, bridge and isolation ---------------------------------------------------------------


BWRAP_PROBE = ["bwrap", "--unshare-all", "--die-with-parent", "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin",
               "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64", "--proc", "/proc", "--dev", "/dev",
               "true"]


def succeeds(argv):
    try:
        return subprocess.run(argv, capture_output=True, timeout=20, check=False).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def bwrap_works():
    return bool(shutil.which("bwrap")) and succeeds(BWRAP_PROBE)


def strace_works():
    """The trace must work around the sandbox itself (a setuid bubblewrap cannot be traced)."""
    return bool(shutil.which("strace")) and succeeds(["strace", "-f", "-qq", "-o", "/dev/null", *BWRAP_PROBE])


BRIDGE_SOCKET = "/run/bridge/cli.sock"
SHIM = '''#!/usr/bin/python3 -I
import json, os, socket, sys
try:
    data = b"" if sys.stdin is None or sys.stdin.isatty() else sys.stdin.buffer.read()
except OSError:
    data = b""
request = {"argv": sys.argv[1:], "cwd": os.getcwd(), "stdin": data.decode("utf-8", "replace")}
connection = socket.socket(socket.AF_UNIX)
connection.connect(SOCKET)
connection.sendall(json.dumps(request).encode())
connection.shutdown(socket.SHUT_WR)
reply = json.loads(b"".join(iter(lambda: connection.recv(65536), b"")))
sys.stdout.write(reply["stdout"])
sys.stderr.write(reply["stderr"])
sys.exit(reply["exit"])
'''


class Sandbox:
    """The participant's shell. `bwrap`: a private /work, read-only /usr and sources, a tmpfs /tmp,
    no network, a cleared environment, and /tools holding either the frozen binary or the bridge
    shim. `none` (development dry runs only, where bubblewrap is unavailable): an ordinary shell in
    the work directory with a cleared environment; isolation is not enforced and is recorded so."""

    def __init__(self, mode, work, tools_dir, tool, binary=None, bridge_dir=None):
        self.mode, self.work, self.tools_dir, self.tool = mode, Path(work), Path(tools_dir), tool
        self.binary, self.bridge_dir = binary, bridge_dir

    def mounts(self):
        argv = ["--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib",
                "--symlink", "usr/lib64", "/lib64", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp",
                "--dir", "/tools", "--bind", str(self.work), "/work"]
        for name in DOCUMENTS:
            path = self.work / name
            if path.is_file() and not path.is_symlink():
                argv += ["--ro-bind", str(path), f"/work/{name}"]
        return argv

    def argv(self, command):
        if self.mode == "none":
            return ["bash", "--noprofile", "--norc", "-c", command]
        argv = ["bwrap", "--unshare-all", "--hostname", "trial", "--die-with-parent", "--new-session", *self.mounts()]
        if self.tool == "binary":
            argv += ["--ro-bind", str(self.binary), "/tools/investigation"]
        elif self.tool == "bridge":
            argv += ["--ro-bind", str(self.tools_dir / "investigation"), "/tools/investigation",
                     "--ro-bind", str(self.bridge_dir), "/run/bridge"]
        argv += ["--clearenv", "--setenv", "HOME", "/work", "--setenv", "PATH", "/tools:/usr/bin",
                 "--setenv", "LC_ALL", "C.UTF-8", "--chdir", "/work"]
        return argv + ["prlimit", FSIZE, NOFILE, "--", "bash", "--noprofile", "--norc", "-c", command]

    def env(self):
        if self.mode != "none":
            return None
        path = (f"{self.tools_dir}:" if self.tool else "") + "/usr/bin:/bin"
        return {"HOME": str(self.work), "PATH": path, "LC_ALL": "C.UTF-8"}

    def run(self, command, timeout, trace=None):
        argv = self.argv(command)
        if trace is not None:
            argv = ["prlimit", "--fsize=8388608", "--", "strace", "-f", "-qq", "-yy", "-s", "128",
                    "--kill-on-exit", "-e", "trace=" + COST["SYSCALLS"], "-o", str(trace), *argv]
        with tempfile.TemporaryFile() as output:
            try:
                child = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=output, stderr=output,
                                       timeout=timeout, cwd=self.work, env=self.env(), check=False)
                exit_code = child.returncode
            except subprocess.TimeoutExpired:
                exit_code = 124
            size = output.tell()
            output.seek(0)
            data = output.read(SHELL_BYTES).decode("utf-8", errors="replace")
        return {"exit": exit_code, "output": data, "bytes": size, "truncated": size > SHELL_BYTES}

    def probe(self, forbidden):
        """Look for forbidden host paths from inside, and compare network namespaces."""
        if self.mode == "none":
            return {"mode": "none", "enforced": False, "visible": None, "network_isolated": False}
        checks = " ".join(f"test -e {shlex.quote(path)} && echo VISIBLE:{shlex.quote(path)};" for path in forbidden)
        result = self.run(f"{checks} readlink /proc/self/ns/net; true", 10)
        visible = re.findall(r"^VISIBLE:(.*)$", result["output"], re.M)
        isolated = os.readlink("/proc/self/ns/net") not in result["output"]
        return {"mode": "bwrap", "enforced": True, "forbidden_checked": len(forbidden), "visible": visible,
                "network_isolated": isolated, "pass": not visible and isolated and result["exit"] == 0}


class _Handler(socketserver.StreamRequestHandler):
    def handle(self):
        request = json.loads(self.rfile.read())
        try:
            code, stdout, stderr = self.server.callback(request)
        except Exception as error:  # noqa: BLE001 - the participant sees a failed call, the log keeps why
            code, stdout, stderr = 70, "", f"investigation: internal harness error: {error}\n"
            self.server.errors.append(repr(error))
        self.wfile.write(json.dumps({"exit": code, "stdout": stdout, "stderr": stderr}).encode())


class _Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


class Bridge:
    """A Unix socket outside the participant's sandbox. The shim at /tools/investigation forwards
    each call here, so the evaluator side can apply a task's faults and read policy and log the
    call while nothing of the evaluator directory is visible to the participant."""

    def __init__(self, directory, callback):
        self.directory = Path(directory)
        self.directory.chmod(0o700)
        self.path = self.directory / "cli.sock"
        self.server = _Server(str(self.path), _Handler)
        self.server.callback = callback
        self.server.errors = []
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.path.unlink(missing_ok=True)
        self.directory.rmdir()


# --- Sessions ------------------------------------------------------------------------------------


def session(run, phase, spec):
    """One fresh participant session: a new session ID and conversation, the frozen budgets, the
    run's sandbox. Ends at submit_answer, the end marker, a returned message or a budget."""
    budget = spec["budget"]
    started = time.monotonic()

    def remaining():
        return budget["seconds"] - (time.monotonic() - started)

    work = run.work
    messages = [{"role": "system", "content": SYSTEM}, {"role": "user", "content": spec["prompt"]}]
    session_id = str(uuid.uuid4())
    counters = Counter({key: 0 for key in ("input_tokens", "output_tokens", "cached_input_tokens_reported",
                                           "model_turns", "shell_calls", "shell_output_bytes",
                                           "truncated_shell_calls", "first_receipt_call", "first_onboard_call",
                                           "first_note_call", *TRACE_FIELDS)})
    cached_reported = False
    status, answer, interruption, injected_turn, turn = None, None, None, None, 0
    trace_dir = run.dir / f"{phase}.trace"
    trace_dir.mkdir()
    journal = COST["journal_events"](work)
    run.phase = phase
    with (run.dir / f"{phase}.jsonl").open("x", encoding="utf-8") as log:
        def record(event, **fields):
            log.write(json.dumps({"event": event, **fields}, ensure_ascii=False) + "\n")
            log.flush()

        record("started", session_id=session_id, phase=phase, role=spec["role"], budget=budget,
               model=run.assignment["model"], messages=messages,
               prompt_sha256=sha_bytes(spec["prompt"].encode()))
        while status is None:
            if remaining() <= 0:
                status = "timed_out"
                break
            if any(counters[key] >= budget[key] for key in ("input_tokens", "output_tokens")):
                status = "budget_exhausted"
                break
            tool_limit = counters["shell_calls"] >= budget["tool_calls"]
            if tool_limit and not spec["answer"]:
                status = "budget_exhausted"
                break
            tools = ([] if tool_limit else [SHELL_TOOL]) + ([SUBMIT] if spec["answer"] else [])
            turn += 1
            allowance = budget["output_tokens"] - counters["output_tokens"]
            options = dict(INFERENCE, seed=run.assignment["block_seed"], num_predict=min(2048, allowance))
            try:
                response = run.backend.chat(run.assignment["model"], messages, tools, options, session_id,
                                            max(0.01, remaining()))
                reply = response["message"]
                cached = response.get("prompt_eval_cached_count")
                counters["input_tokens"] += int(response["prompt_eval_count"]) + int(cached or 0)
                if cached is not None:
                    cached_reported = True
                    counters["cached_input_tokens_reported"] += int(cached)
                counters["output_tokens"] += int(response["eval_count"])
                counters["model_turns"] += 1
            except TimeoutError:
                record("deadline", seconds=budget["seconds"])
                status = "timed_out"
                break
            except (OSError, ValueError, KeyError, TypeError) as error:
                record("backend_error", message=str(error))
                status = "crashed"
                break
            messages.append(reply)
            record("model", response=response)
            if counters["input_tokens"] > budget["input_tokens"] or remaining() <= 0:
                status = "timed_out" if remaining() <= 0 else "budget_exhausted"
                break
            calls = reply.get("tool_calls") or []
            if not calls:
                if spec["answer"]:
                    answer = RUNNER["answer_fields"](reply.get("content", "") or "")
                interruption = "returned_without_marker" if spec.get("end_marker") else "returned"
                status = "completed" if response.get("done_reason") != "length" else "budget_exhausted"
                break
            for call in calls:
                function = call.get("function", {})
                arguments = function.get("arguments", {})
                if isinstance(arguments, str):
                    try:
                        arguments = json.loads(arguments)
                    except ValueError:
                        arguments = {}
                if function.get("name") == "submit_answer" and spec["answer"]:
                    answer = RUNNER["validate_answer"](arguments)
                    if answer is not None:
                        record("submitted", answer=answer)
                        status, interruption = "completed", "submitted"
                        break
                    messages.append({"role": "tool", "tool_name": "submit_answer",
                                     "content": "Supply the four nonempty string fields."})
                    continue
                if counters["shell_calls"] >= budget["tool_calls"]:
                    messages.append({"role": "tool", "tool_name": "shell",
                                     "content": "Shell call budget reached. Submit your answer."})
                    continue
                command = arguments.get("command") if isinstance(arguments, dict) else None
                counters["shell_calls"] += 1
                number = counters["shell_calls"]
                trace = trace_dir / f"{number:04}.log" if run.trace else None
                accounting = {}
                before = spec["before_call"]() if spec.get("before_call") else None
                if function.get("name") != "shell" or not isinstance(command, str) or len(command) > SHELL_BYTES:
                    result = {"exit": 2, "output": "Expected one shell command string.", "bytes": 0, "truncated": False}
                else:
                    run.bridge_mark = len(run.calls)
                    result = run.sandbox.run(command, min(SHELL_SECONDS, max(0.01, remaining())), trace)
                    if trace is not None and trace.exists():
                        with trace.open(encoding="utf-8", errors="replace") as raw:
                            accounting = COST["trace_cost"](raw, work)
                        with trace.open(encoding="utf-8", errors="replace") as raw:
                            run.add_trace_calls(TT810["calls_from_trace"](raw, work), number)
                    for entry in run.calls[run.bridge_mark:]:
                        entry.setdefault("shell_call", number)
                if spec.get("after_call"):
                    replacement = spec["after_call"](before)
                    if replacement is not None:
                        record("fault", shell_call=number, seen=replacement, actual=result)
                        result = dict(replacement, bytes=len(replacement["output"].encode()), truncated=False)
                counters.update(accounting)
                counters["shell_output_bytes"] += result["bytes"]
                counters["truncated_shell_calls"] += int(result.get("truncated", False))
                if accounting.get("onboard_calls") and not counters["first_onboard_call"]:
                    counters["first_onboard_call"] = number
                if accounting.get("note_write_bytes") and not counters["first_note_call"]:
                    counters["first_note_call"] = number
                current = COST["journal_events"](work)
                if current - journal and not counters["first_receipt_call"]:
                    counters["first_receipt_call"] = number
                journal = current
                record("shell", command=command, accounting=accounting,
                       trace=trace.name if trace is not None and trace.exists() else None, **result)
                messages.append({"role": "tool", "tool_name": "shell", "content": json.dumps(result)})
                checkpoint = spec.get("checkpoint")
                if checkpoint and injected_turn is None and RUNNER["checkpoint_marker"](work, "review.ready") \
                        and RUNNER["has_review_material"](work):
                    record("source_injected", sha256=checkpoint())
                    injected_turn = turn
                marker = spec.get("end_marker")
                if marker and RUNNER["checkpoint_marker"](work, marker):
                    if not checkpoint or (injected_turn is not None and injected_turn < turn):
                        status, interruption = "completed", "marker"
                        break
                    (work / marker).unlink()
                    record("premature_handoff")
                if remaining() <= 0:
                    status = "timed_out"
                    break
        elapsed = round((time.monotonic() - started) * 1000)
        record("finished", status=status, elapsed_ms=elapsed, counters=dict(counters))
    run.phase = None
    for field in ("first_receipt_call", "first_onboard_call", "first_note_call"):
        counters[field] = counters[field] or None
    phase_record = {"session_id": session_id, "role": spec["role"], "status": status, "interruption": interruption,
                    "transcript": f"{phase}.jsonl", "prompt_sha256": sha_bytes(spec["prompt"].encode()),
                    "answer_expected": spec["answer"], "cost": run.phase_cost(phase, counters, elapsed, cached_reported)}
    return phase_record, answer


# --- One assignment ------------------------------------------------------------------------------


class Run:
    """One planned assignment: runs/<id>/ with work/ (mounted as /work), evaluator/ (never
    mounted), transcripts, snapshots, the call log and run.json."""

    def __init__(self, trial, assignment):
        self.trial, self.assignment = trial, assignment
        self.dir = trial.dir / "runs" / assignment["id"]
        self.work = self.dir / "work"
        self.evaluator = self.dir / "evaluator"
        self.backend = trial.backend
        self.trace = trial.plan["sandbox"]["trace"] == "strace"
        self.mode = trial.plan["sandbox"]["mode"]
        self.calls = []
        self.lock = threading.Lock()
        self.phase = None
        self.bridge = None
        self.bridge_mark = 0
        self.proxy_plan = None
        self.sandbox = None
        self.cli_meter = None

    # -- tools
    def install_tool(self, arm, proxy_plan=None):
        """No tool in the notes arm. In the CLI arm, the frozen binary itself when the run needs no
        evaluator-side behaviour and the trace can meter it; otherwise the bridge."""
        binary = self.trial.binary
        tools = self.dir / "tools"
        tools.mkdir()
        self.proxy_plan = proxy_plan
        if arm == "notes":
            self.cli_meter = None
            self.sandbox = Sandbox(self.mode, self.work, tools, None)
            return
        if proxy_plan is None and self.mode == "bwrap" and self.trace:
            self.cli_meter = "trace"
            self.sandbox = Sandbox(self.mode, self.work, tools, "binary", binary=binary)
            return
        self.cli_meter = "bridge"
        # A short directory outside the run: Unix socket paths are limited to about 100 bytes.
        bridge_dir = Path(tempfile.mkdtemp(prefix="rtb-"))
        socket_path = BRIDGE_SOCKET if self.mode == "bwrap" else str(bridge_dir / "cli.sock")
        shim = tools / "investigation"
        shim.write_text(SHIM.replace("SOCKET", repr(socket_path)), encoding="utf-8")
        shim.chmod(0o555)
        self.launcher = self.write_launcher(tools)
        if proxy_plan is not None:
            plan = read_json(proxy_plan)
            plan["binary"] = str(self.launcher)
            proxy_plan.write_text(json.dumps(plan, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        self.bridge = Bridge(bridge_dir, self.forward)
        self.sandbox = Sandbox(self.mode, self.work, tools, "bridge", bridge_dir=bridge_dir)

    def write_launcher(self, tools):
        """How the evaluator side runs the binary for a bridged call: in a sandbox with the
        participant's mounts and working directory (bwrap), or directly (none)."""
        if self.mode == "none":
            return self.trial.binary
        sandbox = Sandbox("bwrap", self.work, tools, "binary", binary=self.trial.binary)
        mounts = " ".join(shlex.quote(part) for part in sandbox.mounts())
        work = shlex.quote(str(self.work.resolve()))
        text = f"""#!/bin/bash
here=$(pwd -P)
case "$here" in {work}|{work}/*) inside="/work${{here#{work}}}" ;; *) inside=/work ;; esac
exec bwrap --unshare-all --hostname trial --die-with-parent --new-session {mounts} \\
  --ro-bind {shlex.quote(str(self.trial.binary))} /tools/investigation --clearenv --setenv HOME /work \\
  --setenv PATH /tools:/usr/bin --setenv LC_ALL C.UTF-8 --chdir "$inside" \\
  prlimit {FSIZE} {NOFILE} -- /tools/investigation "$@"
"""
        launcher = self.evaluator / "launcher"
        launcher.write_text(text, encoding="utf-8")
        launcher.chmod(0o500)
        return launcher

    def forward(self, request):
        argv = [str(word) for word in request.get("argv", [])]
        stdin = request.get("stdin") or ""
        cwd = request.get("cwd") or ""
        if self.mode == "bwrap":
            inside = Path(cwd)
            host = self.work / inside.relative_to("/work") if inside.is_relative_to("/work") else self.work
        else:
            host = Path(cwd) if Path(cwd).resolve().is_relative_to(self.work.resolve()) else self.work
        with self.lock:
            if self.proxy_plan is not None:
                code, stdout, stderr = TT["proxy"](self.proxy_plan, argv, stdin, host)
            else:
                env = None if self.mode == "bwrap" else {"HOME": str(self.work), "PATH": "/usr/bin:/bin",
                                                        "LC_ALL": "C.UTF-8"}
                done = subprocess.run([str(self.launcher), *argv], input=stdin, capture_output=True, text=True,
                                      cwd=host, env=env, timeout=SHELL_SECONDS * 4, check=False)
                code, stdout, stderr = done.returncode, done.stdout, done.stderr
            self.calls.append({"argv": ["investigation", *argv], "stdout_bytes": len(stdout.encode()),
                               "exit": code, "phase": self.phase, "via": "bridge"})
        return code, stdout, stderr

    def add_trace_calls(self, entries, number):
        for entry in entries:
            if "argv" in entry and self.cli_meter != "trace":
                continue
            self.calls.append(dict(entry, phase=self.phase, via="trace", shell_call=number))

    def phase_cost(self, phase, counters, elapsed, cached_reported):
        calls = [call for call in self.calls if call.get("phase") == phase]
        per, groups, _ = TT810["costs"](calls, {"raw_files": list(DOCUMENTS)})
        cli = [call for call in calls if "argv" in call]
        cost = {key: counters.get(key) for key in COST_FIELDS if key in counters}
        cost["wall_ms"] = elapsed
        cost["cli_calls"] = len(cli)
        if not self.trace:
            # Without the syscall trace, CLI-level counts come from the bridge log and file-level
            # counts are unknown, never zero.
            discovery = [call for call in cli if TT810["classify"](call["argv"]) == "discovery"]
            cost.update(discovery_calls=len(discovery),
                        discovery_bytes=sum(call["stdout_bytes"] for call in discovery),
                        onboard_calls=sum("onboard" in TT810["words"](call["argv"])[:1] for call in cli),
                        evidence_bytes=None, note_read_bytes=None, note_write_bytes=None, preparation_calls=None)
        first_onboard, first_receipt = cost.get("first_onboard_call"), cost.get("first_receipt_call")
        cost["calls_from_onboard_to_receipt"] = (first_receipt - first_onboard if first_onboard and first_receipt
                                                 and first_receipt >= first_onboard else None)
        cost["read_depth"] = groups
        cost["read_categories"] = per
        cost["metering"] = {
            "tokens": self.trial.plan["backend"]["description"].get("metering", "backend-reported"),
            "cached_input_tokens": "reported" if cached_reported else "not reported by the backend",
            "trace": "strace" if self.trace else None,
            "cli_calls": self.cli_meter or "no tool in this arm",
        }
        for key in COST_FIELDS:
            cost.setdefault(key, None)
        return cost

    # -- isolation
    def isolation(self):
        probe = self.sandbox.probe(self.trial.forbidden + [str(self.evaluator), str(self.trial.dir)])
        scan = self.content_scan()
        probe["content_scan"] = scan
        if scan["matches"] or (probe.get("enforced") and not probe["pass"]):
            raise HarnessError(f"isolation failed for {self.assignment['id']}: {probe}")
        return probe

    def content_scan(self):
        matches, files = [], 0
        for path in sorted(self.work.rglob("*")):
            if path.is_symlink():
                target = os.path.realpath(path)
                if not Path(target).is_relative_to(self.work.resolve()):
                    matches.append({"path": path.relative_to(self.work).as_posix(), "symlink_outside": True})
                continue
            if path.is_file():
                files += 1
                if sha_file(path) in self.trial.key_digests:
                    matches.append({"path": path.relative_to(self.work).as_posix(), "key_bytes": True})
        return {"files": files, "matches": matches}

    # -- the whole assignment
    def execute(self):
        self.dir.mkdir(parents=True)
        started = time.monotonic()
        record = {"schema_version": RUN_SCHEMA, "assignment": self.assignment,
                  "plan_sha256": self.trial.plan_sha256, "started_at": now(),
                  "environment": {"binary_sha256": self.trial.verify_binary(), "model": self.assignment["model"],
                                  "model_identifier": self.trial.plan["backend"]["description"]["models"].get(
                                      self.assignment["model"]),
                                  "backend": self.trial.plan["backend"]["kind"],
                                  "sandbox": self.mode, "trace": self.trial.plan["sandbox"]["trace"]}}
        task_type = self.assignment["task_type"]
        runner = {"family": self.run_family, "lookup": self.run_family}.get(task_type) or (
            self.run_task_5_7 if task_type in ("task-5", "task-6", "task-7") else self.run_task_8_10)
        refused = None
        try:
            details = runner()
            record.update(details)
            record["status"] = status_of(details["phases"])
        except HarnessError as error:
            # An isolation or integrity failure stops the trial after its record is written.
            refused = error
            record.update(status="harness_refused", error=str(error))
        except Exception as error:  # noqa: BLE001 - a crashed assignment is kept and reported
            record.update(status="harness_error", error=f"{type(error).__name__}: {error}")
        finally:
            if self.bridge is not None:
                self.bridge.close()
                record["bridge_errors"] = self.bridge.server.errors
        for name in ("phases", "snapshots", "isolation"):
            record.setdefault(name, {})
        record.setdefault("answer", None)
        record["setup_and_harness_ms"] = round((time.monotonic() - started) * 1000) - sum(
            (phase.get("cost") or {}).get("wall_ms") or 0 for phase in record["phases"].values())
        record["cost"] = {"phases": {name: phase["cost"] for name, phase in record["phases"].items()},
                          "total": total_cost([phase["cost"] for phase in record["phases"].values()])}
        if self.calls:
            with (self.dir / "calls.jsonl").open("x", encoding="utf-8") as log:
                for call in self.calls:
                    log.write(json.dumps(call, sort_keys=True) + "\n")
        record["calls"] = "calls.jsonl" if self.calls else None
        if self.work.is_dir():
            record["snapshots"]["final"] = {"path": "work", "digest": tree_digest(self.work),
                                            "journal_events": len(COST["journal_events"](self.work)),
                                            "notes_files": notes_files(self.work)}
            record["isolation"]["content_scan_final"] = self.content_scan()
        record["finished_at"] = now()
        write_new(self.dir / "run.json", record)
        if refused is not None:
            raise refused
        return record

    def prepare(self, arm, proxy_plan=None):
        self.install_tool(arm, proxy_plan)
        return {"isolation": self.isolation(), "snapshots": {}}

    def spec(self, role, prompt, answer, **extra):
        return dict(role=role, prompt=prompt, answer=answer, budget=self.trial.plan["budgets"][ROLE_BUDGET[role]],
                    **extra)

    def condition_text(self, prompt):
        if self.assignment.get("condition") == "docs":
            return prompt + DOCS_CONTEXT.format(page=self.trial.plan["secondary"]["docs"]["page"].strip())
        return prompt

    def run_family(self):
        a = self.assignment
        cases = self.trial.case_set
        variant = cases.variant(a["family"], a["variant"])
        self.work.mkdir(mode=0o700)
        self.evaluator.mkdir(mode=0o700)
        FIXTURE["stage_source"](cases.source_root, variant, self.work, "initial")
        (self.work / "NOTES.md").touch()
        details = self.prepare(a["arm"])
        objective = cases.objective(a["family"], a["variant"])
        boundary = {"review_status": "missing", "review_digest": None, "update_sha256": None,
                    "interruption": None, "pre_update_operations": {}}

        def checkpoint():
            boundary["review_status"] = "signaled"
            boundary["pre_update_operations"] = RUNNER["operation_counts"](self.work)
            boundary["review_digest"] = tree_digest(self.work)
            shutil.copytree(self.work, self.dir / "review", symlinks=True)
            boundary["update_sha256"] = FIXTURE["stage_source"](cases.source_root, variant, self.work, "update")
            return boundary["update_sha256"]

        phases = {}
        investigate = self.condition_text(f"Objective\n{objective}\n\n{INVESTIGATE}")
        phases["investigator"], _ = session(self, "investigator", self.spec(
            "investigator", investigate, False, end_marker="handoff.ready",
            checkpoint=checkpoint if "update" in variant else None))
        boundary["interruption"] = phases["investigator"]["interruption"] or phases["investigator"]["status"]
        if boundary["review_status"] == "missing" and "update" in variant:
            boundary["pre_update_operations"] = RUNNER["operation_counts"](self.work)
            boundary["update_sha256"] = FIXTURE["stage_source"](cases.source_root, variant, self.work, "update")
        for name in ("review.ready", "handoff.ready"):
            if RUNNER["checkpoint_marker"](self.work, name):
                (self.work / name).unlink()
        boundary["work_digest"] = tree_digest(self.work)
        boundary["handoff_operations"] = RUNNER["operation_counts"](self.work)
        shutil.copytree(self.work, self.dir / "handoff", symlinks=True)
        resume = self.condition_text(f"Objective\n{objective}\n\n{RESUME}")
        phases["successor"], answer = session(self, "successor", self.spec("successor", resume, True))
        write_new(self.dir / "answer.json", answer)
        details["snapshots"]["handoff"] = {"path": "handoff", "digest": boundary["work_digest"],
                                           "journal_events": len(COST["journal_events"](self.dir / "handoff")),
                                           "notes_files": notes_files(self.dir / "handoff")}
        if boundary["review_status"] == "signaled":
            details["snapshots"]["review"] = {"path": "review", "digest": boundary["review_digest"]}
        return dict(details, phases=phases, boundary=boundary, answer="answer.json")

    run_lookup = run_family

    def run_task_5_7(self):
        a = self.assignment
        task = TT["staged"](TT["load_task"](str(resolve_input(a["task"]))), a["stage"])
        visible = "/work" if self.mode == "bwrap" else None
        setup = TT["setup"](task, str(self.trial.binary), self.work, self.evaluator, a["arm"], a["condition"], visible)
        details = self.prepare(a["arm"], self.evaluator / "plan.json" if a["arm"] == "inv" else None)
        details["setup"] = {"evaluator_record": "evaluator/setup.json", "head": setup.get("head")}
        role = task["phase"]["role"]
        prompt = task["phase"]["prompt"][a["arm"]] + "\n\n" + ANSWER_FORMAT
        extra = {}
        faults = setup.get("faults") if a["arm"] == "notes" else None
        if faults:
            notes = self.work / faults["file"]
            state = self.evaluator / "notes-state.json"
            extra["before_call"] = lambda: notes.read_text(encoding="utf-8") if notes.is_file() else ""
            extra["after_call"] = lambda before: TT["notes_fault"](setup, self.work, state, before)
        phase, answer = session(self, "participant", self.spec(role, prompt, True, **extra))
        write_new(self.dir / "answer.json", answer)
        return dict(details, phases={"participant": phase}, answer="answer.json")

    def run_task_8_10(self):
        a = self.assignment
        TT810["setup"](str(resolve_input(a["task"])), self.dir, str(self.trial.binary), a["arm"])
        self.work.chmod(0o700)
        details = self.prepare(a["arm"])
        details["setup"] = {"evaluator_record": "evaluator/baseline.json", "prompt": "prompt.txt"}
        number = a["task_type"].removeprefix("task-")
        role = read_json(TASK_SPEC_8_10)["tasks"][number]["participant"]
        prompt = (self.dir / "prompt.txt").read_text(encoding="utf-8").strip()
        answer_expected = role == "successor"
        spec = self.spec(role, prompt, answer_expected, end_marker=None if answer_expected else "done.ready")
        phase, answer = session(self, "participant", spec)
        if RUNNER["checkpoint_marker"](self.work, "done.ready"):
            (self.work / "done.ready").unlink()
        if answer_expected:
            write_new(self.dir / "answer.json", answer)
        return dict(details, phases={"participant": phase}, answer="answer.json" if answer_expected else None)


def notes_files(root):
    root = Path(root)
    return sorted(path.relative_to(root).as_posix() for path in root.rglob("*.md")
                  if path.is_file() and "state" not in path.relative_to(root).parts)


def status_of(phases):
    statuses = [phase["status"] for phase in phases.values()]
    if all(status == "completed" for status in statuses):
        return "completed"
    return next(status for status in statuses if status != "completed")


def total_cost(costs):
    total = {}
    for key in SUMMED:
        values = [cost.get(key) for cost in costs]
        total[key] = None if any(value is None for value in values) else sum(values)
    depth = {}
    for cost in costs:
        for group, entry in (cost.get("read_depth") or {}).items():
            slot = depth.setdefault(group, {"calls": 0, "bytes": 0})
            slot["calls"] += entry["calls"]
            slot["bytes"] += entry["bytes"]
    total["read_depth"] = depth
    return total


# --- Trial: plan, run, grade, freeze -------------------------------------------------------------


def harness_inputs(case_set):
    """Every repository file the harness reads, pinned by digest at plan time."""
    paths = [Path(__file__), ROOT / "scripts/run-a-gate.py", ROOT / "scripts/grade-a-gate.py",
             ROOT / "scripts/a-gate-cost.py", ROOT / "scripts/check-fixture-families.py",
             ROOT / "scripts/trial-tasks.py", ROOT / "scripts/trial-tasks-8-10.py", REGISTRY, case_set.manifest_path,
             TASK_SPEC_8_10, PREREGISTRATION]
    for task_paths in case_set.trial_tasks.values():
        for path in task_paths:
            file = resolve_input(path)
            paths.append(file)
    return {relative(path): sha_file(path) for path in sorted(set(paths))}


def make_plan(args):
    output = Path(args.output).resolve()
    if args.split == "sealed":
        if output.is_relative_to(ROOT):
            raise HarnessError("a sealed trial directory must be outside the repository")
        if args.backend == "stub":
            raise HarnessError("the stub backend is for development dry runs only")
        if len(args.model) < 2:
            raise HarnessError("the preregistration requires two pinned models")
        if args.trial_tasks is None and not args.families_only:
            raise HarnessError("give --trial-tasks with the sealed task 5-10 instances, or --families-only")
    if len(set(args.model)) != len(args.model):
        raise HarnessError("use distinct model names")
    trial_tasks = read_json(args.trial_tasks) if args.trial_tasks else ({} if args.families_only else None)
    case_set = CaseSet(args.split, args.opened, trial_tasks)
    mode = args.sandbox
    if mode == "auto":
        mode = "bwrap" if bwrap_works() else "none"
    if mode == "bwrap" and not bwrap_works():
        raise HarnessError("bubblewrap is not usable on this host")
    trace = args.trace
    if trace == "auto":
        trace = "strace" if mode == "bwrap" and strace_works() else "none"
    if args.split == "sealed" and (mode != "bwrap" or trace != "strace"):
        raise HarnessError("the sealed trial needs the bubblewrap sandbox and the syscall trace")
    if trace == "strace" and mode != "bwrap":
        raise HarnessError("the trace meters the bubblewrap sandbox only")
    output.mkdir(mode=0o700)
    frozen = output / "frozen"
    frozen.mkdir()
    binary = frozen / "investigation"
    shutil.copyfile(Path(args.bin).resolve(strict=True), binary)
    binary.chmod(0o555)
    backend_config = {"kind": args.backend}
    if args.backend == "command":
        if not args.backend_command:
            raise HarnessError("the command backend needs --backend-command")
        backend_config["argv"] = shlex.split(args.backend_command)
    backend = make_backend(backend_config)
    description = backend.describe(args.model)
    if args.backend == "command":
        backend_config["adapter_sha256"] = description["adapter"]["sha256"]
    backend_config["description"] = description
    secondary = {}
    for name in args.secondary:
        if name not in SECONDARY:
            raise HarnessError(f"unknown secondary condition {name}")
        page = subprocess.run([str(binary), "docs", "agents"], capture_output=True, text=True, check=True,
                              env={"PATH": "/usr/bin", "LC_ALL": "C.UTF-8"}).stdout
        secondary[name] = {"description": SECONDARY[name], "page": page, "page_sha256": sha_bytes(page.encode())}
    unit_list = units(case_set, list(secondary))
    assignments = schedule(unit_list, args.model, args.seed)
    counts = Counter((item["task_type"], item["arm"]) for item in assignments)
    family_assignments = sum(item["task_type"] == "family" and item["primary"] for item in assignments)
    if args.split == "sealed":
        expected = case_set.manifest["preregistration"]["paired_arm_assignments"]
        if len(args.model) == 2 and family_assignments != expected:
            raise HarnessError(f"{family_assignments} primary assignments, the preregistration has {expected}")
    plan = {
        "schema_version": PLAN_SCHEMA, "created_at": now(), "split": args.split, "seed": args.seed,
        "purpose": "dry run: plumbing only, supports no claim" if args.split == "development" else "sealed release trial",
        "preregistration": {"path": relative(PREREGISTRATION), "sha256": sha_file(PREREGISTRATION)},
        "repository_head": git_head(),
        "binary": {"path": "frozen/investigation", "sha256": sha_file(binary), "commit": args.binary_commit,
                   "source": str(Path(args.bin).resolve())},
        "inputs": harness_inputs(case_set),
        "case_set": {"manifest": relative(case_set.manifest_path), "registry": relative(REGISTRY),
                     "opened": str(Path(args.opened).resolve()) if args.opened else None,
                     "keys": {family: entry["key_sha256"] for family, entry in sorted(case_set.entries.items())},
                     "trial_tasks": case_set.trial_tasks},
        "backend": backend_config, "models": args.model,
        "inference": INFERENCE, "budgets": BUDGETS, "role_budgets": ROLE_BUDGET, "shell_seconds": SHELL_SECONDS,
        "shell_output_bytes": SHELL_BYTES,
        "prompts": {"system": SYSTEM, "investigate": INVESTIGATE, "resume": RESUME, "answer_format": ANSWER_FORMAT},
        "sandbox": {"mode": mode, "trace": trace,
                    "tools": {"bubblewrap": version(["bwrap", "--version"]), "strace": version(["strace", "--version"]),
                              "bash": version(["bash", "--version"]), "python": platform.python_version()},
                    "limits": [FSIZE, NOFILE],
                    "isolation": "enforced" if mode == "bwrap" else "not enforced (development dry run without bubblewrap)"},
        "host": {"kernel": platform.release(), "machine": platform.machine(), "system": platform.system()},
        "session_policy": "Every phase is a new session ID and a new conversation seeded only with the system "
                          "prompt and its task prompt; each assignment gets a new work directory; no transcript, "
                          "other assignment, evaluator file or key is visible to a participant.",
        "arms": {"notes": "Markdown notes with ordinary shell and file tools",
                 "inv": "the same plus the frozen binary at /tools/investigation"},
        "secondary": secondary,
        "counts": {"assignments": len(assignments), "primary_family_assignments": family_assignments,
                   "by_task_and_arm": {f"{task}/{arm}": count for (task, arm), count in sorted(counts.items())}},
        "assignments": assignments,
    }
    write_new(output / "plan.json", plan)
    (output / "plan.sha256").write_text(sha_file(output / "plan.json") + "\n", encoding="utf-8")
    (output / "runs").mkdir()
    return output


class Trial:
    def __init__(self, directory, opened=None, backend_command=None, backend=True):
        self.dir = Path(directory).resolve()
        if (self.dir / "frozen.json").exists():
            raise HarnessError("this trial is frozen; its records cannot change")
        self.plan_sha256 = sha_file(self.dir / "plan.json")
        if self.plan_sha256 != (self.dir / "plan.sha256").read_text().strip():
            raise HarnessError("plan.json changed after it was frozen")
        self.plan = read_json(self.dir / "plan.json")
        opened = opened or self.plan["case_set"]["opened"]
        tasks = self.plan["case_set"]["trial_tasks"]
        self.case_set = CaseSet(self.plan["split"], opened, tasks)
        for path, digest in self.plan["inputs"].items():
            if sha_file(resolve_input(path)) != digest:
                raise HarnessError(f"harness input changed since the plan was frozen: {path}")
        self.binary = self.dir / self.plan["binary"]["path"]
        self.verify_binary()
        self.backend = make_backend(self.plan["backend"], backend_command) if backend else None
        self.key_digests = self.case_set.key_digests()
        self.forbidden = self.case_set.forbidden_paths()

    def verify_binary(self):
        digest = sha_file(self.binary)
        if digest != self.plan["binary"]["sha256"]:
            raise HarnessError("the frozen binary changed")
        return digest

    def deviation(self, kind, **fields):
        with (self.dir / "deviations.jsonl").open("a", encoding="utf-8") as log:
            log.write(json.dumps({"at": now(), "kind": kind, **fields}, sort_keys=True) + "\n")


def run_trial(directory, only=(), parallel=1, opened=None, backend_command=None, quiet=False):
    trial = Trial(directory, opened, backend_command)
    if parallel != 1 and trial.plan["split"] != "development":
        raise HarnessError("sealed assignments run serially")
    pending = []
    for assignment in trial.plan["assignments"]:
        if only and assignment["id"] not in only:
            continue
        run_dir = trial.dir / "runs" / assignment["id"]
        if (run_dir / "run.json").is_file():
            continue
        if run_dir.exists():
            aborted = trial.dir / "aborted"
            aborted.mkdir(exist_ok=True)
            target = aborted / f"{assignment['id']}-{len(list(aborted.glob(assignment['id'] + '-*'))) + 1}"
            run_dir.rename(target)
            trial.deviation("aborted_run_moved", assignment=assignment["id"], moved_to=relative(target))
        pending.append(assignment)

    def one(assignment):
        record = Run(trial, assignment).execute()
        if not quiet:
            print(json.dumps({"id": assignment["id"], "task": assignment["task_type"], "status": record["status"]}),
                  flush=True)
        return record

    if parallel == 1:
        results = [one(assignment) for assignment in pending]
    else:
        with ThreadPoolExecutor(max_workers=parallel) as pool:
            results = list(pool.map(one, pending))
    trial.verify_binary()
    return results


# --- Grading -------------------------------------------------------------------------------------


def answer_key(trial, assignment):
    """(key ID, rubric, documents) for an assignment's answer, or None when the task has no answer."""
    task_type = assignment["task_type"]
    cases = trial.case_set
    if task_type in ("family", "lookup"):
        key = cases.key(assignment["family"])
        documents = {name: data for name in DOCUMENTS
                     if (data := cases.document(assignment["family"], assignment["variant"], name)) is not None}
        return f"{assignment['family']}/{assignment['variant']}", key["variants"][assignment["variant"]], documents
    if task_type in ("task-5", "task-6", "task-7"):
        task = TT["staged"](TT["load_task"](str(resolve_input(assignment["task"]))), assignment["stage"])
        key = read_json(resolve_input(task["key"]))
        spec = task["arms"].get("inv", {})
        directory = TT["source_dir"](task, spec)
        documents = {name: (directory / name).read_bytes() for name in DOCUMENTS if (directory / name).is_file()}
        return f"{task['id']}/{assignment['variant']}", key["variants"][assignment["variant"]], documents
    if task_type == "task-8":
        instance = read_json(resolve_input(assignment["task"]))
        documents = {}
        run_dir = trial.dir / "runs" / assignment["id"]
        for name in instance["key"].get("source_files", []):
            for candidate in (run_dir / "work" / name, run_dir / "work/casework" / name):
                if candidate.is_file():
                    documents[name] = candidate.read_bytes()
                    break
        return instance["id"], instance["key"]["answer_rubric"], documents
    return None


def normalized_answer(run_dir, answer, documents):
    if answer is None:
        return None, {}
    aliases = GRADER["mappings"](run_dir / "work", documents)
    citations = GRADER["select_citations"](answer["citation"], documents, aliases)
    return {field: GRADER["clean_field"](answer[field]) for field in ("answer", "limitation", "next")}, citations


def mechanics(trial, assignment, record, scratch, answer_row):
    """Tool and task mechanics, graded on a scratch copy so the run's own snapshot stays as left."""
    task_type = assignment["task_type"]
    run_dir = trial.dir / "runs" / assignment["id"]
    work = run_dir / "work"
    binary = str(trial.binary)
    if task_type in ("family", "lookup"):
        events = len(COST["journal_events"](work)) if work.is_dir() else 0
        result = {"task": task_type, "arm": assignment["arm"],
                  "review_checkpoint": (record.get("boundary") or {}).get("review_status"),
                  "interruption": (record.get("boundary") or {}).get("interruption"),
                  "journal_events": events, "notes_files": notes_files(work) if work.is_dir() else []}
        if task_type == "lookup":
            result["unnecessary_tool_use"] = assignment["arm"] == "inv" and events > 0
        return result
    copy_root = scratch / assignment["id"]
    if task_type in ("task-5", "task-6", "task-7"):
        shutil.copytree(work, copy_root / "work", symlinks=True)
        task = TT["staged"](TT["load_task"](str(resolve_input(assignment["task"]))), assignment["stage"])
        setup = read_json(run_dir / "evaluator/setup.json")
        calls = TT["read_calls"](run_dir / "evaluator/calls.jsonl")
        result = TT["grade"](task, setup, copy_root / "work", calls, binary)
        result.update(stage=assignment["stage"], condition=assignment["condition"])
        return result
    shutil.copytree(work, copy_root / "work", symlinks=True)
    shutil.copytree(run_dir / "evaluator", copy_root / "evaluator", symlinks=True)
    calls = read_calls(run_dir / "calls.jsonl")
    try:
        return TT810["grade"](copy_root, binary, calls, answer_row if task_type == "task-8" else None)
    except TT810["Failed"] as error:
        return {"task": task_type, "passed": False, "error": str(error)}


def read_calls(path):
    path = Path(path)
    if not path.is_file():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def grade_trial(directory, opened=None, seed=8103):
    trial = Trial(directory, opened, None, backend=False)
    grades = trial.dir / "grades"
    grades.mkdir()
    planned = {assignment["id"]: assignment for assignment in trial.plan["assignments"]}
    records = {}
    for run_json in sorted((trial.dir / "runs").glob("*/run.json")):
        record = read_json(run_json)
        if record["plan_sha256"] != trial.plan_sha256:
            raise HarnessError(f"{run_json} belongs to another plan")
        records[record["assignment"]["id"]] = record
    sessions = Counter(phase["session_id"] for record in records.values() for phase in record["phases"].values())
    reused = sorted(session_id for session_id, count in sessions.items() if count > 1)
    rows, mapping, keys = [], {}, {}
    scratch = Path(tempfile.mkdtemp(prefix="release-trial-grading-"))
    try:
        normalized = {}
        for assignment_id, assignment in planned.items():
            found = answer_key(trial, assignment)
            if found is None:
                continue
            key_id, rubric, documents = found
            keys[key_id] = rubric
            record = records.get(assignment_id)
            answer = None
            run_dir = trial.dir / "runs" / assignment_id
            if record is not None and record.get("answer") and (run_dir / "answer.json").is_file():
                answer = read_json(run_dir / "answer.json")
            fields, citations = normalized_answer(run_dir, answer, documents)
            normalized[assignment_id] = {"answer": fields, "citations": citations}
            response_id = str(uuid.uuid4())
            rows.append({"id": response_id, "key": key_id, "answer": fields, "citations": citations})
            mapping[response_id] = assignment_id
        random.Random(seed).shuffle(rows)
        write_new(grades / "blinded.json", rows)
        scored = [dict(GRADER["score"](row, keys[row["key"]]), key=row["key"]) for row in rows]
        write_new(grades / "answer-grades.json", scored)
        write_new(grades / "assignment-map.json", mapping)
        by_assignment = {mapping[item["id"]]: item for item in scored}
        (grades / "mechanics").mkdir()
        mechanics_results = {}
        for assignment_id, record in sorted(records.items()):
            assignment = planned[assignment_id]
            try:
                result = mechanics(trial, assignment, record, scratch, normalized.get(assignment_id))
            except Exception as error:  # noqa: BLE001 - a grading failure is recorded, not hidden
                result = {"task": assignment["task_type"], "grading_error": f"{type(error).__name__}: {error}"}
            mechanics_results[assignment_id] = result
            write_new(grades / "mechanics" / f"{assignment_id}.json", result)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    graded = []
    for assignment_id, assignment in planned.items():
        record = records.get(assignment_id)
        answer_grade = by_assignment.get(assignment_id)
        mech = mechanics_results.get(assignment_id)
        graded.append({
            "assignment": assignment, "status": record["status"] if record else "not_run",
            "answer_grade": None if answer_grade is None else {
                key: answer_grade[key] for key in ("correct", "unwarranted", "accuracy", "missed_warranted",
                                                   "false_promotion_rate")},
            "mechanics_passed": None if mech is None else mech_pass(mech),
            "mechanics": f"mechanics/{assignment_id}.json" if mech is not None else None,
            "cost_total": record["cost"]["total"] if record else None,
        })
    write_new(grades / "graded-runs.json", graded)
    reconcile = reconciliation(trial, planned, records, reused)
    write_new(grades / "reconcile.json", reconcile)
    write_new(grades / "summary.json", summary(graded))
    frozen = freeze(trial.dir)
    return {"graded": len(graded), "reconcile": reconcile, "frozen_digest": frozen["digest"]}


def mech_pass(result):
    for key in ("pass", "passed"):
        if key in result:
            return result[key]
    return None


def reconciliation(trial, planned, records, reused):
    statuses = Counter(record["status"] for record in records.values())
    missing = sorted(set(planned) - set(records))
    aborted = sorted(path.name for path in (trial.dir / "aborted").glob("*")) if (trial.dir / "aborted").is_dir() else []
    deviations = read_calls(trial.dir / "deviations.jsonl")
    return {"planned": len(planned), "attempted": len(records), "not_run": missing, "statuses": dict(statuses),
            "aborted_attempts": aborted, "deviations": deviations, "reused_session_ids": reused,
            "complete": not missing and not reused}


def summary(graded):
    groups = {}
    for row in graded:
        a = row["assignment"]
        key = (a["task_type"], a["arm"], a.get("stage"), a.get("condition"))
        group = groups.setdefault(key, {"planned": 0, "completed": 0, "answers_graded": 0, "accuracy_sum": 0.0,
                                        "false_promotion_runs": 0, "mechanics_graded": 0, "mechanics_passed": 0,
                                        "wall_ms": [], "input_tokens": [], "output_tokens": [], "shell_calls": [],
                                        "cli_calls": [], "discovery_bytes": []})
        group["planned"] += 1
        group["completed"] += row["status"] == "completed"
        if row["answer_grade"] is not None:
            group["answers_graded"] += 1
            group["accuracy_sum"] += row["answer_grade"]["accuracy"]
            group["false_promotion_runs"] += any(row["answer_grade"]["unwarranted"].values())
        if row["mechanics_passed"] is not None:
            group["mechanics_graded"] += 1
            group["mechanics_passed"] += bool(row["mechanics_passed"])
        for field in ("wall_ms", "input_tokens", "output_tokens", "shell_calls", "cli_calls", "discovery_bytes"):
            value = (row["cost_total"] or {}).get(field)
            if value is not None:
                group[field].append(value)
    rows = []
    for (task_type, arm, stage, condition), group in sorted(groups.items(), key=lambda item: [str(v) for v in item[0]]):
        answers = group.pop("answers_graded")
        accuracy = group.pop("accuracy_sum")
        row = {"task_type": task_type, "arm": arm, "stage": stage, "condition": condition, **{
            key: value for key, value in group.items() if not isinstance(value, list)}}
        row["answers_graded"] = answers
        row["mean_accuracy"] = round(accuracy / answers, 4) if answers else None
        row["mean_cost"] = {field: (round(sum(values) / len(values), 1) if values else None)
                            for field, values in group.items() if isinstance(values, list)}
        rows.append(row)
    return {"note": "Descriptive counts with denominators. The preregistered analysis (paired, "
                    "family-clustered intervals) is W14.13 and reads graded-runs.json.", "rows": rows}


# --- Freezing ------------------------------------------------------------------------------------


def manifest_of(directory):
    directory = Path(directory)
    files = {}
    for path in sorted(directory.rglob("*")):
        name = path.relative_to(directory).as_posix()
        if name == "frozen.json":
            continue
        if path.is_symlink():
            files[name] = "symlink:" + os.readlink(path)
        elif path.is_file():
            files[name] = sha_file(path)
    return files


def freeze(directory):
    """Hash every file of the trial, write frozen.json, and make the whole tree read-only. The
    harness refuses to run or grade a frozen trial; `verify` detects any later edit."""
    directory = Path(directory)
    files = manifest_of(directory)
    digest = sha_bytes(json.dumps(files, sort_keys=True).encode())
    frozen = {"schema_version": FROZEN_SCHEMA, "frozen_at": now(), "plan_sha256": sha_file(directory / "plan.json"),
              "files": len(files), "digest": digest, "manifest": files}
    write_new(directory / "frozen.json", frozen)
    for path in sorted(directory.rglob("*"), key=lambda item: len(item.parts), reverse=True):
        if path.is_symlink():
            continue
        mode = path.stat().st_mode
        path.chmod(stat.S_IMODE(mode) & ~0o222)
    directory.chmod(stat.S_IMODE(directory.stat().st_mode) & ~0o222)
    return frozen


def verify(directory):
    directory = Path(directory)
    frozen = read_json(directory / "frozen.json")
    files = manifest_of(directory)
    digest = sha_bytes(json.dumps(files, sort_keys=True).encode())
    expected = frozen["manifest"]
    changed = sorted(name for name in expected.keys() & files.keys() if expected[name] != files[name])
    return {"ok": digest == frozen["digest"] and not changed and files.keys() == expected.keys(),
            "digest": digest, "frozen_digest": frozen["digest"], "changed": changed,
            "missing": sorted(expected.keys() - files.keys()), "added": sorted(files.keys() - expected.keys())}


# --- Dry run and self-test -----------------------------------------------------------------------


def dry_run(binary, output, models=("dry-run-a",), seed=1405, parallel=1, sandbox="auto", trace="auto", quiet=True):
    args = argparse.Namespace(split="development", bin=binary, binary_commit=git_head(), output=output,
                              model=list(models), seed=seed, backend="stub", backend_command=None, opened=None,
                              trial_tasks=None, families_only=False, secondary=[], sandbox=sandbox, trace=trace)
    directory = make_plan(args)
    run_trial(directory, parallel=parallel, quiet=quiet)
    graded = grade_trial(directory)
    return directory, graded


def schema_check(value, schema, definitions=None, path="$"):
    """The subset of JSON Schema the run-record schema uses: type, required, properties, enum, $ref."""
    definitions = definitions or schema.get("$defs", {})
    if "$ref" in schema:
        return schema_check(value, definitions[schema["$ref"].split("/")[-1]], definitions, path)
    errors = []
    kinds = schema.get("type")
    if kinds:
        kinds = kinds if isinstance(kinds, list) else [kinds]
        names = {"object": dict, "array": list, "string": str, "integer": int, "number": (int, float),
                 "boolean": bool, "null": type(None)}
        if not any(isinstance(value, names[kind]) and not (kind in ("integer", "number") and isinstance(value, bool))
                   for kind in kinds):
            return [f"{path}: expected {kinds}, got {type(value).__name__}"]
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{path}: {value!r} not in {schema['enum']}")
    if isinstance(value, dict):
        for name in schema.get("required", []):
            if name not in value:
                errors.append(f"{path}: missing {name}")
        for name, child in schema.get("properties", {}).items():
            if name in value:
                errors += schema_check(value[name], child, definitions, f"{path}.{name}")
        extra = schema.get("additionalProperties")
        if isinstance(extra, dict):
            for name, child in value.items():
                if name not in schema.get("properties", {}):
                    errors += schema_check(child, extra, definitions, f"{path}.{name}")
    return errors


def self_test(binary, keep=None, sandbox="auto"):
    """Unit checks, then a full dry run on the development set. Prints one JSON report."""
    checks, failed = [], []

    def check(name, condition, detail=None):
        checks.append(name)
        if not condition:
            failed.append({"check": name, "detail": detail})

    # Schedule: seeded, counterbalanced, twins opposite, every unit once per model.
    cases = CaseSet("development")
    unit_list = units(cases, ["docs"])
    plan_a = schedule(copy.deepcopy(unit_list), ["m1", "m2"], 7)
    plan_b = schedule(copy.deepcopy(unit_list), ["m1", "m2"], 7)
    check("schedule_reproducible_from_seed", plan_a == plan_b)
    check("schedule_differs_by_seed", plan_a != schedule(copy.deepcopy(unit_list), ["m1", "m2"], 8))
    kinds = {item["task_type"] for item in plan_a}
    check("every_task_type_scheduled", kinds == set(TASK_TYPES), sorted(kinds))
    domains = {item["domain"] for item in plan_a if item["task_type"] == "family"}
    check("four_domains", domains == {"debugging", "measurement", "source-research", "architecture"}, sorted(domains))
    family_rows = [item for item in plan_a if item["task_type"] == "family" and item["primary"]]
    per_family = Counter((item["family"], item["arm"], item["order"]) for item in family_rows)
    check("family_order_balanced", all(per_family[(family, arm, 1)] == per_family[(family, arm, 2)]
                                       for family, arm, _ in per_family), dict(per_family))
    firsts = {}
    for item in family_rows:
        if item["order"] == 1:
            firsts[(item["family"], item["model"], item["twin"] == "original")] = item["arm"]
    check("twin_takes_opposite_order", all(firsts[(family, model, True)] != firsts[(family, model, False)]
                                           for family, model, _ in firsts))
    check("both_arms_for_paired_units", all(
        {row["arm"] for row in plan_a if row["block"] == item["block"]} == {"notes", "inv"}
        for item in plan_a if item["task_type"] in ("family", "lookup", "task-5", "task-6", "task-8")
        and item["condition"] is None))
    check("secondary_conditions_not_primary", all(not item["primary"] for item in plan_a if item["condition"] == "docs")
          and any(item["condition"] == "docs" for item in plan_a))
    # The sealed design from its public commitments alone (no content is read): 672 assignments.
    sealed = types.SimpleNamespace(manifest={"families": read_json(SEALED_MANIFEST)["families"]},
                                   families={family["id"]: family for family in read_json(REGISTRY)["families"]},
                                   trial_tasks={})
    sealed_rows = schedule(units(sealed), ["m1", "m2"], 3)
    expected = read_json(SEALED_MANIFEST)["preregistration"]["paired_arm_assignments"]
    check("sealed_design_matches_preregistration", len(sealed_rows) == expected, len(sealed_rows))
    # Dry-run keys: every reference answer scores fully, every overclaim is flagged.
    for family, entry in sorted(cases.entries.items()):
        key = cases.key(family)
        for variant_id, rubric in key["variants"].items():
            documents = {name: data for name in DOCUMENTS if (data := cases.document(family, variant_id, name))}
            for answer in rubric["reference_answers"]:
                citations = GRADER["select_citations"](answer["citation"], documents, {})
                scored = GRADER["score"]({"id": "x", "answer": answer, "citations": citations}, rubric)
                check(f"key_reference_scores:{family}/{variant_id}",
                      scored["accuracy"] == 1 and not any(scored["unwarranted"].values()), scored)
            over = rubric["overclaim_answer"]
            scored = GRADER["score"]({"id": "x", "answer": over, "citations": {}}, rubric)
            check(f"key_overclaim_flagged:{family}/{variant_id}", any(scored["unwarranted"].values()), scored)
    # Stub plays both arms alike.
    listing_cli = "total 1\nsource.txt\n/tools/investigation\n"
    listing_notes = "total 1\nsource.txt\nNOTES.md\nno investigation command\n"
    check("stub_writes_in_both_arms", len(Stub.plan(INVESTIGATE, listing_cli)) == len(Stub.plan(INVESTIGATE, listing_notes)))

    scratch = Path(keep) if keep else Path(tempfile.mkdtemp(prefix="release-trial-self-test-"))
    report = {}
    try:
        # The isolation checks are not vacuous: a visible path and a copied key are both caught.
        work = scratch / "probe-work"
        work.mkdir(parents=True)
        if sandbox != "none" and bwrap_works():
            probe = Sandbox("bwrap", work, scratch, None).probe(["/usr", str(ROOT)])
            check("probe_detects_a_visible_path", probe["visible"] == ["/usr"] and not probe["pass"], probe)
        # The adapter protocol, with a minimal adapter: describe, then one chat turn.
        adapter = scratch / "adapter.py"
        adapter.write_text(ADAPTER_EXAMPLE, encoding="utf-8")
        backend = CommandBackend([sys.executable, "-I", str(adapter)])
        described = backend.describe(["m1"])
        reply = backend.chat("m1", [{"role": "system", "content": "s"}, {"role": "user", "content": "u"}],
                             [SUBMIT], dict(INFERENCE), "session", 30)
        check("adapter_protocol", described["models"] == {"m1": "example:m1"} and described["adapter"]["sha256"]
              and RUNNER["validate_answer"](reply["message"]["tool_calls"][0]["function"]["arguments"]), reply)
        key_file = next((DRY_RUN / "keys").glob("*.json"))
        shutil.copyfile(key_file, work / "copied.json")
        holder = types.SimpleNamespace(work=work, trial=types.SimpleNamespace(key_digests=cases.key_digests()))
        check("content_scan_detects_a_key", Run.content_scan(holder)["matches"] == [
            {"path": "copied.json", "key_bytes": True}])
        directory, graded = dry_run(binary, scratch / "trial", parallel=4, sandbox=sandbox)
        plan = read_json(directory / "plan.json")
        schema = read_json(ROOT / "training/release-trial/run-record.schema.json")
        records = {path.parent.name: read_json(path) for path in (directory / "runs").glob("*/run.json")}
        check("every_assignment_ran", set(records) == {item["id"] for item in plan["assignments"]})
        covered = Counter((record["assignment"]["task_type"], record["assignment"]["arm"]) for record in records.values()
                          if record["status"] == "completed")
        for task_type in TASK_TYPES:
            arms = {arm for (kind, arm) in covered if kind == task_type}
            expected = {"inv"} if task_type in ("task-9", "task-10") else {"inv", "notes"}
            check(f"dry_run_completes:{task_type}", arms == expected, sorted(arms))
        for run_id, record in sorted(records.items()):
            run_dir = directory / "runs" / run_id
            errors = schema_check(record, schema)
            check(f"run_record_schema:{run_id}", not errors, errors[:5])
            for name, phase in record["phases"].items():
                check(f"transcript:{run_id}/{name}", (run_dir / phase["transcript"]).is_file())
                missing = [key for key in COST_FIELDS if key not in phase["cost"]]
                metered = [key for key in ("wall_ms", "input_tokens", "output_tokens", "shell_calls", "model_turns",
                                           "cli_calls") if phase["cost"][key] is None]
                check(f"cost_complete:{run_id}/{name}", not missing and not metered, missing + metered)
                if plan["sandbox"]["trace"] == "strace":
                    check(f"cost_traced:{run_id}/{name}", all(phase["cost"][key] is not None for key in TRACE_FIELDS))
            if record["phases"].get("participant", record["phases"].get("successor", {})).get("answer_expected"):
                check(f"answer:{run_id}", (run_dir / "answer.json").is_file() and read_json(run_dir / "answer.json"))
            check(f"snapshot:{run_id}", record["snapshots"]["final"]["digest"] and (
                record["snapshots"]["final"]["journal_events"] or record["snapshots"]["final"]["notes_files"]))
            if record["assignment"]["arm"] == "inv":
                check(f"cli_used:{run_id}", record["cost"]["total"]["cli_calls"] > 0, record["cost"]["total"])
            isolation = record["isolation"]
            check(f"no_key_in_work:{run_id}", not isolation["content_scan"]["matches"]
                  and not isolation["content_scan_final"]["matches"])
            if isolation["enforced"]:
                check(f"keys_not_visible:{run_id}", isolation["pass"] and isolation["forbidden_checked"] > 0, isolation)
        family = [record for record in records.values() if record["assignment"]["task_type"] == "family"
                  and record["assignment"]["arm"] == "inv"]
        check("review_checkpoint_staged_update", all(record["boundary"]["review_status"] == "signaled"
                                                     and record["boundary"]["update_sha256"] for record in family))
        check("handoff_snapshot_has_journal", all(record["snapshots"]["handoff"]["journal_events"] for record in family))
        notes6 = [record for record in records.values() if record["assignment"]["task_type"] == "task-6"
                  and record["assignment"]["arm"] == "notes"]
        check("task6_notes_fault_fired", all('"event": "fault"' in (directory / "runs" / record["assignment"]["id"] /
                                                                    "participant.jsonl").read_text() for record in notes6))
        inv6 = [record for record in records.values() if record["assignment"]["task_type"] == "task-6"
                and record["assignment"]["arm"] == "inv"]
        check("task6_bridge_logged_proxy_calls", all((directory / "runs" / record["assignment"]["id"] /
                                                      "evaluator/calls.jsonl").is_file() for record in inv6))
        check("graded_and_reconciled", graded["reconcile"]["complete"], graded["reconcile"])
        grades = read_json(directory / "grades/graded-runs.json")
        answered = [row for row in grades if row["answer_grade"] is not None]
        check("answers_graded_blind", answered and all("arm" not in row and "model" not in row
                                                      for row in read_json(directory / "grades/blinded.json")))
        check("mechanics_graded", all(row["mechanics"] for row in grades))
        errors = [name for name in (directory / "grades/mechanics").iterdir() if "grading_error" in read_json(name)]
        check("mechanics_without_errors", not errors, [path.name for path in errors])
        verified = verify(directory)
        check("frozen_verifies", verified["ok"], verified)
        try:
            Trial(directory)
            check("frozen_refuses_rerun", False)
        except HarnessError:
            check("frozen_refuses_rerun", True)
        check("frozen_read_only", not os.access(directory / "runs", os.W_OK) and not os.access(
            next((directory / "runs").glob("*/run.json")), os.W_OK))
        # Tampering is detected.
        victim = next((directory / "runs").glob("*/run.json"))
        victim.chmod(0o644)
        victim.write_text(victim.read_text() + " ")
        check("tampering_detected", not verify(directory)["ok"] and verify(directory)["changed"])
        report = {"trial": str(directory), "runs": len(records), "covered": sorted(f"{t}/{a}" for t, a in covered),
                  "sandbox": plan["sandbox"]["mode"], "trace": plan["sandbox"]["trace"],
                  "summary": read_json(directory / "grades/summary.json")["rows"]}
    finally:
        if not keep:
            for path in scratch.rglob("*"):
                if not path.is_symlink():
                    path.chmod(0o700 if path.is_dir() else 0o600)
            shutil.rmtree(scratch, ignore_errors=True)
    return {"checks": len(checks), "failed": failed, **report}


# --- Command line --------------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    plan = commands.add_parser("plan", help="freeze inputs and write the seeded schedule")
    plan.add_argument("--split", choices=("development", "sealed"), required=True)
    plan.add_argument("--bin", required=True, help="the release-candidate binary; a copy is frozen into the trial")
    plan.add_argument("--binary-commit", required=True)
    plan.add_argument("--output", required=True)
    plan.add_argument("--model", action="append", required=True)
    plan.add_argument("--seed", type=int, required=True)
    plan.add_argument("--backend", choices=("stub", "command", "local"), required=True)
    plan.add_argument("--backend-command", help="the adapter command line for --backend command")
    plan.add_argument("--opened", help="sealed: the opened bundle directory, outside the repository")
    plan.add_argument("--trial-tasks", help="sealed: JSON {task number: [instance paths]} for tasks 5-10")
    plan.add_argument("--families-only", action="store_true", help="sealed: plan families and lookups only")
    plan.add_argument("--secondary", action="append", default=[], choices=sorted(SECONDARY))
    plan.add_argument("--sandbox", choices=("auto", "bwrap", "none"), default="auto")
    plan.add_argument("--trace", choices=("auto", "strace", "none"), default="auto")
    run = commands.add_parser("run", help="run planned assignments that have no record yet")
    run.add_argument("trial")
    run.add_argument("--only", action="append", default=[])
    run.add_argument("--parallel", type=int, default=1)
    run.add_argument("--opened")
    run.add_argument("--backend-command", help="the same adapter at another path; its digest must match")
    grade = commands.add_parser("grade", help="blind, grade, reconcile and freeze")
    grade.add_argument("trial")
    grade.add_argument("--opened")
    check = commands.add_parser("verify", help="check a frozen trial against frozen.json")
    check.add_argument("trial")
    dry = commands.add_parser("dry-run", help="plan, run and grade the development set with the stub")
    dry.add_argument("--bin", required=True)
    dry.add_argument("--output", required=True)
    dry.add_argument("--model", action="append")
    dry.add_argument("--parallel", type=int, default=1)
    dry.add_argument("--sandbox", choices=("auto", "bwrap", "none"), default="auto")
    test = commands.add_parser("self-test")
    test.add_argument("--bin", required=True)
    test.add_argument("--keep", help="keep the dry-run trial in this new directory")
    test.add_argument("--sandbox", choices=("auto", "bwrap", "none"), default="auto",
                      help="none exercises the fallback used where bubblewrap is unavailable")
    args = parser.parse_args()
    try:
        if args.command == "plan":
            print(make_plan(args))
        elif args.command == "run":
            run_trial(args.trial, set(args.only), args.parallel, args.opened, args.backend_command)
        elif args.command == "grade":
            print(json.dumps(grade_trial(args.trial, args.opened), indent=2))
        elif args.command == "verify":
            result = verify(args.trial)
            print(json.dumps(result, indent=2))
            sys.exit(0 if result["ok"] else 1)
        elif args.command == "dry-run":
            directory, graded = dry_run(args.bin, args.output, args.model or ["dry-run-a", "dry-run-b"],
                                        parallel=args.parallel, sandbox=args.sandbox, quiet=False)
            print(json.dumps({"trial": str(directory), **graded}, indent=2))
        else:
            report = self_test(args.bin, args.keep, args.sandbox)
            print(json.dumps(report, indent=2))
            sys.exit(1 if report["failed"] else 0)
    except HarnessError as error:
        print(f"release-trial: {error}", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
