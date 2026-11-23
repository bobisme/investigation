#!/usr/bin/env python3
"""Compare two pinned CLI builds on cold first use and a source-linked handoff.

Each assignment gives an investigator one local source in an ordinary working
directory and the `investigation` executable on PATH, then gives a fresh
successor the resulting directory. Both arms use the same isolated shell,
session loop, prompts and budgets as the A-gate runner; only the executable
differs. Model runs are manual. CI runs `scripts/check-first-use.py`, which
uses a stub backend.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import random
import re
import runpy
import shutil
import subprocess
import uuid


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
KEYS = ROOT / "training/first-use/keys"
RUNNER = runpy.run_path(str(ROOT / "scripts/run-a-gate.py"))
GRADER = runpy.run_path(str(ROOT / "scripts/grade-a-gate.py"))
FIXTURE = runpy.run_path(str(ROOT / "scripts/check-fixture-families.py"))
COST = runpy.run_path(str(ROOT / "scripts/a-gate-cost.py"))
# The A-gate session loop reads its prompts and shell from its module globals.
# Patch that namespace, not the copy runpy returns.
LOOP = RUNNER["session"].__globals__
ARMS = ("current", "revised")
BUDGETS = RUNNER["BUDGETS"]
INVESTIGATE = """Investigate the objective using the evidence in this directory. The team keeps
investigation records with the `investigation` command on PATH. Keep your durable work in
it so that an unfamiliar successor can resume from those records. Preserve the decisive
citation and the strongest limitation. Your chat will not be handed over. When your work
is ready for handoff, create an empty file named handoff.ready."""
RESUME = RUNNER["RESUME"] + """
The previous investigator kept records with the `investigation` command on PATH."""
CLI_ERROR = re.compile(
    r'"ok":\s*false|^error:|Retry or inspect with|See investigation schema|no investigation\.toml found'
    r'|^/[a-z_]+(?:/\d+)?: ', re.M)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def persisted(work, digest):
    """Return workspaces under work that hold a case and the source's exact bytes."""
    found = []
    for config in sorted(work.rglob("investigation.toml")):
        state = config.parent / "state"
        blob = state / "blobs/sha256" / digest[:2] / digest
        cases = [case for case in state.glob("cases/*/genesis.json") if case.is_file()]
        if blob.is_file() and not blob.is_symlink() and cases and sha(blob) == digest:
            found.append(config.parent.relative_to(work).as_posix() or ".")
    return found


def invoked_cli(trace):
    """True when the trace shows a successful exec of the participant CLI."""
    with trace.open(encoding="utf-8", errors="replace") as lines:
        return any(call.startswith('execve("/tools/investigation"') and value == 0
                   for _, call, value in COST["events"](lines))


class Probe:
    """Observe each participant call: first CLI contact and first persisted source plus case."""

    def __init__(self, digest):
        self.digest = digest
        self.calls = 0
        self.first_contact_call = None
        self.source_case_call = None
        self.cli_calls = 0

    def __call__(self, command, work, binary, timeout, trace=None):
        result = SHELL(command, work, binary, timeout, trace)
        self.calls += 1
        invoked = trace is not None and trace.exists() and invoked_cli(trace)
        if invoked:
            self.cli_calls += 1
            self.first_contact_call = self.first_contact_call or self.calls
        if self.source_case_call is None and persisted(work, self.digest):
            self.source_case_call = self.calls
        return result


SHELL = LOOP["shell"]


def session(model, phase, variant, work, binary, transcript, budget, seed, backend):
    probe = Probe(variant["initial"]["sha256"])
    saved = {name: LOOP[name] for name in ("shell", "INVESTIGATE", "RESUME")}
    LOOP.update(shell=probe, INVESTIGATE=INVESTIGATE, RESUME=RESUME)
    boundary = {"review_status": "missing", "interruption": None}
    try:
        counters, answer = RUNNER["session"](model, phase, variant, work, binary, transcript,
                                             budget, seed, boundary, backend)
    finally:
        LOOP.update(saved)
    counters.update(first_contact_call=probe.first_contact_call, source_case_call=probe.source_case_call,
                    cli_calls=probe.cli_calls, interruption=boundary["interruption"])
    return counters, answer


def friction(transcript):
    """Classify CLI invocations in a transcript; error detection is a text heuristic."""
    calls = []
    for line in transcript.read_text(encoding="utf-8").splitlines():
        event = json.loads(line)
        if event["event"] != "shell":
            continue
        cli = "investigation" in (event.get("command") or "")
        calls.append({"cli": cli, "error": bool(cli and CLI_ERROR.search(event["output"])),
                      "timeout": event["exit"] == 124})
    return {"cli_error_calls": sum(call["cli"] and call["error"] for call in calls),
            "shell_timeouts": sum(call["timeout"] for call in calls)}


def grade(answer, key, handoff):
    documents = {"source.txt": (handoff / "source.txt").read_bytes()}
    aliases = GRADER["mappings"](handoff, documents)
    citations = GRADER["select_citations"](answer["citation"], documents, aliases) if answer else {}
    row = {"id": "row", "answer": answer, "citations": citations}
    scored = GRADER["score"](row, key)
    return {"correct": scored["correct"], "unwarranted": scored["unwarranted"], "citations": citations}


def assignment(block, arm, variant, key, output, binary, environment, budgets=BUDGETS,
               backend=RUNNER["request"]):
    run_id = str(uuid.uuid4())
    run = output / run_id
    work = run / "workspace"
    work.mkdir(parents=True, mode=0o700)
    FIXTURE["stage_source"](FIXTURES, variant, work, "initial")
    model, seed = block["model"], block["block_seed"]
    phases = {}
    phases["investigator"], _ = session(model, "investigator", variant, work, binary,
                                        run / "investigator.jsonl", budgets["investigator"], seed, backend)
    marker = work / "handoff.ready"
    if marker.is_file() and not marker.is_symlink():
        marker.unlink()
    for name in ("review.ready",):
        if (work / name).is_file():
            (work / name).unlink()
    handoff = {"persisted_workspaces": persisted(work, variant["initial"]["sha256"]),
               "operations": RUNNER["operation_counts"](work), "digest": RUNNER["tree_digest"](work)}
    shutil.copytree(work, run / "handoff", symlinks=True)
    phases["successor"], answer = session(model, "successor", variant, work, binary,
                                          run / "successor.jsonl", budgets["successor"], seed, backend)
    for phase in phases:
        phases[phase].update(friction(run / f"{phase}.jsonl"))
    graded = grade(answer, key, run / "handoff")
    data = {key_: block[key_] for key_ in ("family", "variant", "domain", "block_seed", "model")}
    data.update(id=run_id, arm=arm, order=block["order"].index(arm) + 1)
    record = {"assignment": data, "environment": dict(environment, arm_binary=environment["binaries"][arm]),
              "budgets": budgets, "handoff": handoff, "phases": phases, "answer": answer, "grade": graded}
    RUNNER["write_json"](run / "run.json", record)
    print(json.dumps(dict(data, source_case=phases["investigator"]["source_case_call"],
                          persisted=bool(handoff["persisted_workspaces"]), answer=answer is not None,
                          correct=graded["correct"])), flush=True)
    return record


def schedule(families, models, arms, seed):
    """Counterbalance build order within each model across matched cases."""
    generator = random.Random(seed)
    blocks = []
    for model_index, model in enumerate(models):
        cases = [(family, variant) for family in families for variant in family["variants"]]
        generator.shuffle(cases)
        offset = generator.randrange(2)
        for index, (family, variant) in enumerate(cases):
            order = list(arms)
            if len(order) == 2 and (index + model_index + offset) % 2:
                order.reverse()
            blocks.append({"family": family["id"], "variant": variant["id"], "domain": family["domain"],
                           "model": model, "block_seed": generator.randrange(1, 2**31), "order": order})
    return blocks


def load_key(family):
    key = json.loads((KEYS / f"{family}.json").read_text(encoding="utf-8"))
    return key


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--current", type=Path, required=True, help="Executable built from main at the start.")
    parser.add_argument("--current-commit", required=True)
    parser.add_argument("--revised", type=Path)
    parser.add_argument("--revised-commit")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model", action="append", required=True)
    parser.add_argument("--role", choices=("tuning", "comparison"), default="comparison")
    parser.add_argument("--family", action="append", default=[])
    parser.add_argument("--arm", action="append", choices=ARMS)
    parser.add_argument("--seed", type=int, default=51701)
    args = parser.parse_args()
    arms = tuple(args.arm or ARMS)
    binaries = {"current": args.current.resolve(strict=True)}
    commits = {"current": args.current_commit}
    if "revised" in arms:
        if args.revised is None or args.revised_commit is None:
            parser.error("The revised arm needs --revised and --revised-commit.")
        binaries["revised"] = args.revised.resolve(strict=True)
        commits["revised"] = args.revised_commit
    for binary in binaries.values():
        RUNNER["check_isolation"](binary)
    registry = json.loads((FIXTURES / "family-registry.json").read_text(encoding="utf-8"))
    FIXTURE["validate"](registry, FIXTURES)
    keys = {path.stem: load_key(path.stem) for path in sorted(KEYS.glob("*.json"))}
    families = [family for family in registry["families"]
                if family["id"] in keys and keys[family["id"]]["role"] == args.role
                and (not args.family or family["id"] in args.family)]
    if not families or any(family["split"] != "development" for family in families):
        parser.error("Select registered development families with first-use keys.")
    installed = {model["name"]: model["digest"] for model in RUNNER["request"]("/api/tags")["models"]}
    environment = {
        "binaries": {arm: {"path": binaries[arm].name, "sha256": sha(binaries[arm]), "commit": commits[arm]}
                     for arm in arms},
        "runner_sha256": sha(Path(__file__)), "a_gate_runner_sha256": sha(ROOT / "scripts/run-a-gate.py"),
        "fixture_commit": subprocess.check_output(["git", "-C", ROOT, "rev-parse", "HEAD"], text=True).strip(),
        "provider": "local-ollama", "models": {name: installed[name] for name in args.model},
        "inference": dict(RUNNER["INFERENCE"], think=True),
        "host": {"kernel": platform.release(), "machine": platform.machine()},
        "tools": {"ollama": RUNNER["request"]("/api/version")["version"], "python": platform.python_version()},
    }
    blocks = schedule(families, args.model, arms, args.seed)
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    RUNNER["write_json"](output / "manifest.json", {
        "created_at": datetime.now(timezone.utc).isoformat(), "role": args.role, "seed": args.seed,
        "arms": arms, "environment": environment, "budgets": BUDGETS, "blocks": blocks,
        "prompts": {"system": RUNNER["SYSTEM"], "investigator": INVESTIGATE, "successor": RESUME}})
    variants = {(family["id"], variant["id"]): variant for family in families for variant in family["variants"]}
    for model in args.model:
        warm = RUNNER["request"]("/api/generate", {"model": model, "prompt": "", "stream": False,
                                                  "options": RUNNER["INFERENCE"], "keep_alive": "10m"}, timeout=180)
        RUNNER["write_json"](output / f"warm-{args.model.index(model)}.json", warm)
        for block in (block for block in blocks if block["model"] == model):
            variant = variants[(block["family"], block["variant"])]
            key = keys[block["family"]]["variants"][block["variant"]]
            for arm in block["order"]:
                assignment(block, arm, variant, key, output, binaries[arm], environment)
        # Unload before the next model so only one model occupies the shared GPU.
        RUNNER["request"]("/api/generate", {"model": model, "prompt": "", "stream": False, "keep_alive": 0},
                          timeout=60)
    for arm in arms:
        if sha(binaries[arm]) != environment["binaries"][arm]["sha256"]:
            raise RuntimeError(f"The {arm} executable changed during the run.")


if __name__ == "__main__":
    main()
