#!/usr/bin/env python3
"""Run the W17.03 docs and help-only arms through a hosted agent (bn-gef3).

The local-model runner (`run-first-use.py`) drives a raw model through its own
tool loop. Here the participant is an agent host with its own tool loop:
Claude Code in print mode (`claude -p --model haiku`). The runner starts the
host inside the same kind of bubblewrap sandbox the local runner uses for
each shell call, so that the host and every command it runs see only:

- /work: the assignment's working directory (source.txt read-only),
- /tools/investigation: the pinned executable,
- /opt/host/claude: the host executable,
- /home/agent: a fresh, empty home that holds only a copy of the access token
  (no refresh token), removed when the session ends on any path; a session
  refuses to start while an earlier copy remains, and every run ends by
  checking that no copy remains and no output holds a credential,
- /usr, and the few /etc files that name resolution and TLS need.

No owner configuration reaches the host: no ~/.claude (CLAUDE.md, memory,
skills, plugins, settings), no ~/.claude.json (MCP servers), no ~/.codex and no
repository. The host runs with only its Bash tool, no MCP servers
(`--strict-mcp-config` with an empty config), no slash commands or skills, and
no session persistence. `probe` checks that, by asking the host to report
every instruction it was given beyond the prompt, and by listing the
directories it can see.

The docs arm gets the build's agent instructions (`investigation docs agents`)
as /work/AGENTS.md, read-only, for both the investigator and the successor.
Claude Code loads that file from its working directory as project
instructions (its built-in AGENTS.md plugin; the probe shows it). The
help-only arm gets none.

The prompts are the local runner's, with its system prompt appended to the
host's. The successor's answer, a submit_answer tool call in the local runner,
is the last JSON object with the four fields in the host's final message;
that one sentence of the successor prompt asks for the object instead of
submit_answer. (The host's own structured output, --json-schema, failed on
a pilot: the model packed all four fields into one as tags, five times.) A
PreToolUse hook refuses shell calls past the call budget with the local
runner's message, so the session goes on to its answer.

Calls are the host's Bash tool calls. CLI contact is read from an execve trace
of the whole sandbox (strace -f), split per call at the times the host
returned each result. A persisted Source and Case is checked after every tool
result, as in the local runner.

Model runs are manual; nothing here runs in CI.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import re
import runpy
import shutil
import signal
import subprocess
import threading
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
GATE = runpy.run_path(str(ROOT / "scripts/run-a-gate.py"))
FIRST = runpy.run_path(str(ROOT / "scripts/run-first-use.py"))
FIXTURE = runpy.run_path(str(ROOT / "scripts/check-fixture-families.py"))
COST = GATE["COST"]

HOST_BINARY = Path(shutil.which("claude") or "claude")
CREDENTIALS = Path.home() / ".claude/.credentials.json"
HOMES = Path(os.environ.get("TMPDIR", "/tmp")) / "first-use-hosted-homes"
BUDGETS = GATE["BUDGETS"]
# The local runner's per-command shell limit, applied through the host's own
# Bash timeout settings.
COMMAND_SECONDS = 12
SUBMIT_FROM = "Use submit_answer with four nonempty fields:"
SUBMIT_TO = "End your final message with one JSON object of four nonempty string fields:"
EXECVE = re.compile(r"^(\d+)\s+(\d+\.\d+)\s+(.*)$")
BUDGET_REACHED = "Shell call budget reached. Submit your answer."
# A PreToolUse hook refuses calls past the budget with the local runner's
# message, so the session goes on to its answer, as in the local runner.
HOOK = f"""#!/bin/bash
cat > /dev/null
count=$(( $(cat /opt/host-state/calls 2>/dev/null || echo 0) + 1 ))
echo "$count" > /opt/host-state/calls
if [ "$count" -gt "$(cat /opt/host-state/budget)" ]; then
  echo '{BUDGET_REACHED}' >&2
  exit 2
fi
"""
SETTINGS = {"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
    {"type": "command", "command": "/opt/host-state/budget-hook"}]}]}}
# Past the budget, a host that keeps calling is stopped after this many refusals.
REFUSALS = 4
ETC = ("/etc/resolv.conf", "/etc/hosts", "/etc/nsswitch.conf", "/etc/ssl", "/etc/ca-certificates")


def prompts(arm):
    """The local runner's prompts; the successor's answer tool becomes a JSON object in its final message."""
    texts = FIRST["prompts"]("help-only")
    assert SUBMIT_FROM in texts["successor"]
    return {"investigator": texts["investigator"],
            "successor": texts["successor"].replace(SUBMIT_FROM, SUBMIT_TO)}


def token_copies():
    """Token copies left under HOMES, by an earlier session that did not clean up."""
    return sorted(HOMES.rglob(".credentials.json")) if HOMES.exists() else []


def make_home(home):
    """A fresh home with only the current access token: no refresh token, so the
    session can never rotate the owner's login. Refuses to start while an
    earlier copy remains under HOMES."""
    leftover = token_copies()
    if leftover:
        raise RuntimeError(f"A token copy from an earlier session remains: {leftover[0]}; remove it first.")
    token = json.loads(CREDENTIALS.read_text(encoding="utf-8"))["claudeAiOauth"]
    if token["expiresAt"] / 1000 - time.time() < 900:
        raise RuntimeError("The access token expires within 15 minutes; refresh the login first.")
    (home / ".claude").mkdir(parents=True, mode=0o700)
    kept = {key: token[key] for key in ("accessToken", "expiresAt", "scopes", "subscriptionType") if key in token}
    descriptor = os.open(home / ".claude/.credentials.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        json.dump({"claudeAiOauth": kept}, stream)
    return home


def secrets():
    token = json.loads(CREDENTIALS.read_text(encoding="utf-8"))["claudeAiOauth"]
    return [token[key] for key in ("accessToken", "refreshToken") if token.get(key)]


def make_state(state, budget):
    state.mkdir(mode=0o700)
    (state / "budget").write_text(f"{budget}\n", encoding="utf-8")
    hook = state / "budget-hook"
    hook.write_text(HOOK, encoding="utf-8")
    hook.chmod(0o500)
    return state


def remove(*directories):
    for directory in directories:
        if directory.exists():
            shutil.rmtree(directory)


def verify_clean(output):
    """Exit nonzero when a token copy remains under HOMES or a credential value
    appears in any file under output."""
    problems = [f"token copy left at {path}" for path in token_copies()]
    problems += [f"credential found in {path}" for path in (leaked(output) if output.exists() else [])]
    if problems:
        raise SystemExit("Credential check failed: " + "; ".join(problems) + ". Do not archive this run.")


def sandbox(work, binary, home, host, state):
    argv = ["bwrap", "--unshare-all", "--share-net", "--hostname", "a-gate", "--die-with-parent",
            "--new-session", "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin",
            "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64",
            "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--dir", "/tools"]
    for path in ETC:
        argv += ["--ro-bind", path, path]
    argv += ["--bind", str(home), "/home/agent", "--bind", str(work), "/work",
             "--bind", str(state), "/opt/host-state",
             "--ro-bind", str(state / "budget-hook"), "/opt/host-state/budget-hook",
             "--ro-bind", str(state / "budget"), "/opt/host-state/budget"]
    for name in ("source.txt", "update.txt", "AGENTS.md"):
        path = work / name
        if path.is_file() and not path.is_symlink():
            argv += ["--ro-bind", str(path), f"/work/{name}"]
    if binary is not None:
        argv += ["--ro-bind", str(binary), "/tools/investigation"]
    argv += ["--ro-bind", str(host), "/opt/host/claude", "--clearenv",
             "--setenv", "HOME", "/home/agent", "--setenv", "USER", "agent",
             "--setenv", "PATH", "/tools:/usr/bin", "--setenv", "LC_ALL", "C.UTF-8",
             "--setenv", "BASH_DEFAULT_TIMEOUT_MS", str(COMMAND_SECONDS * 1000),
             "--setenv", "BASH_MAX_TIMEOUT_MS", str(COMMAND_SECONDS * 1000),
             "--setenv", "DISABLE_AUTOUPDATER", "1", "--chdir", "/work"]
    return argv


def final_answer(text):
    """The last JSON object in the host's final message that has exactly the
    four nonempty answer fields, or None."""
    decoder, found = json.JSONDecoder(), None
    for start in (index for index, char in enumerate(text or "") if char == "{"):
        try:
            value, _ = decoder.raw_decode(text, start)
        except ValueError:
            continue
        answer = GATE["validate_answer"](value)
        if answer is not None:
            found = answer
    return found


def host_command(model, prompt):
    argv = ["prlimit", "--fsize=2097152", "--", "/opt/host/claude", "-p", "--model", model,
            "--output-format", "stream-json", "--verbose", "--tools", "Bash",
            "--permission-mode", "bypassPermissions", "--strict-mcp-config",
            "--mcp-config", '{"mcpServers":{}}', "--disable-slash-commands",
            "--no-session-persistence", "--settings", json.dumps(SETTINGS),
            "--append-system-prompt", GATE["SYSTEM"]]
    return argv + [prompt]


def reader(stream, lines):
    for line in iter(stream.readline, b""):
        lines.put((time.time(), line.decode("utf-8", errors="replace")))
    lines.put((time.time(), None))


def stop(process):
    if process.poll() is not None:
        return
    for sig, wait in ((signal.SIGTERM, 5), (signal.SIGKILL, 5)):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            return
        try:
            process.wait(timeout=wait)
            return
        except subprocess.TimeoutExpired:
            continue


def text_of(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(part.get("text", "") for part in content if isinstance(part, dict))
    return ""


def split_trace(trace, calls, directory):
    """Write one strace-format log per call (timestamps removed), holding the
    execve events after the previous call's result up to this call's result."""
    directory.mkdir()
    events = []
    if trace.exists():
        for line in trace.read_text(encoding="utf-8", errors="replace").splitlines():
            match = EXECVE.match(line)
            if match:
                events.append((float(match.group(2)), f"{match.group(1)} {match.group(3)}"))
    previous = float("-inf")
    for number, call in enumerate(calls, start=1):
        end = call.get("ended", float("inf"))
        lines = [text for at, text in events if previous < at <= end]
        previous = max(previous, end)
        (directory / f"{number:04}.log").write_text("".join(f"{text}\n" for text in lines), encoding="utf-8")


def session(model, phase, variant, work, binary, transcript, budget, texts, home_name):
    """One host session; returns counters and the answer (successor only)."""
    prompt = f'Objective\n{variant["objective"]}\n\n{texts[phase]}'
    home = HOMES / home_name
    state = home.with_name(home.name + "-state")
    process = None
    # The token copy is removed on every exit path, including a crash or an
    # interrupt, and the host's whole process group is stopped first.
    try:
        make_home(home)
        make_state(state, budget["tool_calls"])
        trace = transcript.with_suffix(".strace")
        host_log = transcript.with_suffix(".host.jsonl")
        argv = ["strace", "-f", "-qq", "-ttt", "-s", "128", "--kill-on-exit", "-e", "trace=execve",
                "-o", str(trace), *sandbox(work, binary, home, HOST_BINARY.resolve(), state),
                *host_command(model, prompt)]
        started = time.time()
        deadline = started + budget["seconds"]
        calls, pending, status, answer, result, init = [], {}, None, None, None, None
        source_case_call, journal = None, COST["journal_events"](work)
        first_receipt_call, marker = None, False
        lines = queue.Queue()
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        thread = threading.Thread(target=reader, args=(process.stdout, lines), daemon=True)
        thread.start()
        with transcript.open("x", encoding="utf-8") as log, host_log.open("x", encoding="utf-8") as raw:
            def record(event, **fields):
                log.write(json.dumps({"event": event, **fields}, ensure_ascii=False) + "\n")
                log.flush()

            record("started", phase=phase, prompt=prompt, model=model)
            while status is None:
                try:
                    at, line = lines.get(timeout=max(0.05, deadline - time.time()))
                except queue.Empty:
                    status = "timed_out"
                    break
                if line is None:
                    status = "completed" if result is not None else "crashed"
                    break
                raw.write(line)
                raw.flush()
                try:
                    event = json.loads(line)
                except ValueError:
                    record("host_text", text=line.rstrip())
                    continue
                kind = event.get("type")
                if kind == "system" and event.get("subtype") == "init":
                    init = {key: event.get(key) for key in (
                        "model", "claude_code_version", "tools", "mcp_servers", "skills", "plugins", "agents",
                        "slash_commands", "memory_paths", "permissionMode", "cwd", "apiKeySource")}
                    record("host_init", **init)
                elif kind == "assistant":
                    for part in event.get("message", {}).get("content", []):
                        if part.get("type") != "tool_use":
                            continue
                        if part.get("name") != "Bash":
                            record("host_tool", tool=part.get("name"), input=part.get("input"))
                            continue
                        command = (part.get("input") or {}).get("command")
                        calls.append({"id": part.get("id"), "tool": part.get("name"), "command": command,
                                      "started": at})
                        pending[part.get("id")] = len(calls) - 1
                elif kind == "user":
                    for part in event.get("message", {}).get("content", []):
                        if part.get("type") != "tool_result" or part.get("tool_use_id") not in pending:
                            continue
                        index = pending.pop(part["tool_use_id"])
                        call = calls[index]
                        call["ended"] = at
                        output = text_of(part.get("content"))
                        number = index + 1
                        if part.get("is_error") and BUDGET_REACHED in output:
                            call["refused"] = True
                            record("refused", command=call["command"], output=output)
                            if sum(c.get("refused", False) for c in calls) > REFUSALS:
                                status = "budget_exhausted"
                                break
                            continue
                        if source_case_call is None and FIRST["persisted"](work, variant["initial"]["sha256"]):
                            source_case_call = number
                        current = COST["journal_events"](work)
                        if current - journal and first_receipt_call is None:
                            first_receipt_call = number
                        journal = current
                        record("shell", command=call["command"], tool=call["tool"],
                               exit=1 if part.get("is_error") else 0, output=output,
                               bytes=len(output.encode()), truncated=False, trace=f"{number:04}.log")
                        if phase == "investigator" and GATE["checkpoint_marker"](work, "handoff.ready"):
                            status, marker = "completed", True
                            record("handoff_marker")
                            break
                elif kind == "result":
                    result = event
                    if phase == "successor":
                        answer = final_answer(event.get("result"))
                    record("host_result", subtype=event.get("subtype"), is_error=event.get("is_error"),
                           num_turns=event.get("num_turns"), duration_ms=event.get("duration_ms"),
                           usage=event.get("usage"), model_usage=event.get("modelUsage"),
                           result=event.get("result"), answer=answer)
            stop(process)
            elapsed_ms = round((time.time() - started) * 1000)
            refused = sum(call.get("refused", False) for call in calls)
            calls = [call for call in calls if not call.get("refused")]
            record("finished", status=status, elapsed_ms=elapsed_ms, tool_calls=len(calls), refused_calls=refused)
        thread.join(timeout=5)
    finally:
        if process is not None:
            stop(process)
        remove(home, state)
    split_trace(trace, calls, transcript.with_suffix(".trace"))
    traces = transcript.with_suffix(".trace")
    first_contact = next((number for number in range(1, len(calls) + 1)
                          if FIRST["invoked_cli"](traces / f"{number:04}.log")), None)
    onboard = next((number for number in range(1, len(calls) + 1)
                    if COST["trace_cost"]((traces / f"{number:04}.log").read_text().splitlines(), work)
                    .get("onboard_calls")), None)
    usage = (result or {}).get("usage") or {}
    counters = {
        "status": status, "interruption": "handoff_marker" if marker else None,
        "tool_calls": len(calls), "refused_calls": refused, "elapsed_ms": elapsed_ms,
        "input_tokens": usage.get("input_tokens"), "output_tokens": usage.get("output_tokens"),
        "cache_read_input_tokens": usage.get("cache_read_input_tokens"),
        "cache_creation_input_tokens": usage.get("cache_creation_input_tokens"),
        "first_contact_call": first_contact, "first_onboard_call": onboard,
        "first_receipt_call": first_receipt_call, "source_case_call": source_case_call,
        "cli_calls": sum(FIRST["invoked_cli"](traces / f"{n:04}.log") for n in range(1, len(calls) + 1)),
        "shell_timeouts": None, "discovery_bytes": None, "host": init,
        "transcript": transcript.name, "host_transcript": host_log.name,
    }
    if phase == "investigator" and counters["interruption"] is None and status == "completed":
        counters["interruption"] = "returned_without_handoff"
    return counters, answer


def assignment(block, arm, variant, key, output, binary, environment, texts):
    run_id = str(uuid.uuid4())
    run = output / run_id
    work = run / "workspace"
    work.mkdir(parents=True, mode=0o700)
    FIXTURE["stage_source"](FIXTURES, variant, work, "initial")
    if arm == "docs":
        (work / "AGENTS.md").write_text(environment["docs_page"]["text"], encoding="utf-8")
    model = environment["models"][block["model"]]["model"]
    phases = {}
    phases["investigator"], _ = session(model, "investigator", variant, work, binary,
                                        run / "investigator.jsonl", BUDGETS["investigator"], texts,
                                        f"{run_id}-investigator")
    marker = work / "handoff.ready"
    if marker.is_file() and not marker.is_symlink():
        marker.unlink()
    handoff = {"persisted_workspaces": FIRST["persisted"](work, variant["initial"]["sha256"]),
               "operations": GATE["operation_counts"](work), "digest": GATE["tree_digest"](work)}
    shutil.copytree(work, run / "handoff", symlinks=True)
    phases["successor"], answer = session(model, "successor", variant, work, binary,
                                          run / "successor.jsonl", BUDGETS["successor"], texts,
                                          f"{run_id}-successor")
    for phase in phases:
        phases[phase].update(FIRST["friction"](run / f"{phase}.jsonl"))
    graded = FIRST["grade"](answer, key, run / "handoff")
    data = {key_: block[key_] for key_ in ("family", "variant", "domain", "block_seed", "model")}
    data.update(id=run_id, arm=arm, order=block["order"].index(arm) + 1)
    page = {k: v for k, v in environment["docs_page"].items() if k != "text"}
    record = {"assignment": data,
              "environment": dict({k: v for k, v in environment.items() if k != "docs_page"},
                                  docs_page=page, arm_binary=environment["binaries"][arm]),
              "budgets": BUDGETS, "handoff": handoff, "phases": phases, "answer": answer, "grade": graded}
    GATE["write_json"](run / "run.json", record)
    print(json.dumps(dict(data, source_case=phases["investigator"]["source_case_call"],
                          persisted=bool(handoff["persisted_workspaces"]), answer=answer is not None,
                          correct=graded["correct"])), flush=True)
    return record


def leaked(output):
    """Paths of files under output that contain a credential value."""
    values = [value.encode() for value in secrets()]
    found = []
    for path in sorted(output.rglob("*")):
        if path.is_file() and not path.is_symlink():
            data = path.read_bytes()
            if any(value in data for value in values):
                found.append(str(path.relative_to(output)))
    return found


PROBE = ("Before doing anything else, and without running any tool: list every instruction or piece of "
         "context you were given other than this message and your tool definitions. For each, give its "
         "source (system prompt, CLAUDE.md, AGENTS.md, memory, skill, MCP server, plugin, hook or other) "
         "and quote its first line. Say explicitly whether you can see any CLAUDE.md, AGENTS.md, user "
         "memory, skills or MCP servers. Then run exactly one command, `ls -la / /work /tools ~ ~/.claude; "
         "ls /home/bob ~/.codex 2>&1; env | sort`, and report its output.")
TOKEN = re.compile(r"(MESSAGING_TOKEN=)[0-9a-f]+")


def probe(binary, model, output, page):
    """Isolation probe: one help-only and one docs session, each reporting what
    the host loaded. Raises when the host reports any tool other than Bash, an
    MCP server, a skill or slash command, or sees the owner's home."""
    output.mkdir(mode=0o700)
    report = {}
    for arm in ("help-only", "docs"):
        work = output / arm
        work.mkdir(mode=0o700)
        (work / "source.txt").write_text("probe source\n", encoding="utf-8")
        if arm == "docs":
            (work / "AGENTS.md").write_text(page, encoding="utf-8")
        variant = {"objective": "Isolation probe.", "initial": {"sha256": "0" * 64}}
        counters, _ = session(model, "investigator", variant, work, binary, output / f"{arm}.jsonl",
                              {"seconds": 120, "tool_calls": 3}, {"investigator": PROBE}, f"probe-{arm}")
        events = [json.loads(line) for line in (output / f"{arm}.jsonl").read_text().splitlines()]
        listing = next((e.get("output") for e in events if e["event"] == "shell"), "") or ""
        report[arm] = {"host": counters["host"], "status": counters["status"],
                       "answer": next((e.get("result") for e in events if e["event"] == "host_result"), None),
                       "listing": TOKEN.sub(r"\1(redacted)", listing)}
        host = counters["host"] or {}
        problems = [name for name, ok in (
            ("tools", host.get("tools") == ["Bash"]), ("mcp_servers", host.get("mcp_servers") == []),
            ("skills", host.get("skills") == []), ("slash_commands", host.get("slash_commands") == []),
            ("plugins", all(p.get("path") == "builtin" for p in host.get("plugins") or [])),
            ("owner_home", "cannot access '/home/bob'" in listing),
            ("codex_home", "cannot access '/home/agent/.codex'" in listing)) if not ok]
        report[arm]["problems"] = problems
    report["credential_leaks"] = leaked(output)
    GATE["write_json"](output / "probe.json", report)
    if report["credential_leaks"] or any(report[arm]["problems"] for arm in ("help-only", "docs")):
        raise SystemExit(f"Isolation probe failed; see {output / 'probe.json'}.")
    return report


def run(args):
    binary = args.binary.resolve(strict=True)
    GATE["check_isolation"](binary)
    page = subprocess.check_output([binary, "docs", "agents"], text=True,
                                   env={"PATH": "/usr/bin", "LC_ALL": "C.UTF-8"})
    texts = prompts("help-only")
    registry = json.loads((FIXTURES / "family-registry.json").read_text(encoding="utf-8"))
    FIXTURE["validate"](registry, FIXTURES)
    keys = {path.stem: FIRST["load_key"](path.stem) for path in sorted(FIRST["KEYS"].glob("*.json"))}
    families = [family for family in registry["families"]
                if family["id"] in keys and keys[family["id"]]["role"] == "comparison"]
    assert len(families) == 6 and all("update" not in v for f in families for v in f["variants"])
    replicates = [f"{args.host_label}-{suffix}" for suffix in "ab"]
    version = subprocess.check_output([HOST_BINARY, "--version"], text=True).strip()
    environment = {
        "binaries": {arm: {"path": binary.name, "sha256": FIRST["sha"](binary), "commit": args.binary_commit}
                     for arm in ("docs", "help-only")},
        "runner_sha256": FIRST["sha"](Path(__file__)),
        "local_runner_sha256": FIRST["sha"](ROOT / "scripts/run-first-use.py"),
        "a_gate_runner_sha256": FIRST["sha"](ROOT / "scripts/run-a-gate.py"),
        "fixture_commit": subprocess.check_output(["git", "-C", ROOT, "rev-parse", "HEAD"], text=True).strip(),
        "provider": "claude-code",
        "models": {label: {"host": "claude", "host_version": version, "model": args.model} for label in replicates},
        "host_sha256": FIRST["sha"](HOST_BINARY.resolve()),
        "host": {"kernel": platform.release(), "machine": platform.machine()},
        "tools": {"python": platform.python_version(),
                  "bubblewrap": subprocess.check_output(["bwrap", "--version"], text=True).strip(),
                  "strace": subprocess.check_output(["strace", "-V"], text=True).splitlines()[0]},
        "docs_page": {"argv": ["investigation", "docs", "agents"], "file": "/work/AGENTS.md",
                      "sha256": hashlib.sha256(page.encode()).hexdigest(), "bytes": len(page.encode()),
                      "text": page},
    }
    blocks = FIRST["schedule"](families, replicates, ("docs", "help-only"), args.seed)
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    report = probe(binary, args.model, output / "probe", page)
    environment["probe"] = {arm: {"host": report[arm]["host"], "problems": report[arm]["problems"]}
                            for arm in ("help-only", "docs")}
    GATE["write_json"](output / "manifest.json", {
        "created_at": datetime.now(timezone.utc).isoformat(), "role": "comparison", "seed": args.seed,
        "arms": ["docs", "help-only"],
        "environment": {k: v for k, v in environment.items() if k != "docs_page"}
        | {"docs_page": {k: v for k, v in environment["docs_page"].items() if k != "text"}},
        "budgets": BUDGETS, "command_seconds": COMMAND_SECONDS, "blocks": blocks,
        "prompts": {"system": GATE["SYSTEM"], "appended_to_host_system_prompt": True, **texts},
        "host_argv": host_command(args.model, "PROMPT")})
    variants = {(f["id"], v["id"]): v for f in families for v in f["variants"]}
    for block in blocks:
        for arm in block["order"]:
            key = keys[block["family"]]["variants"][block["variant"]]
            assignment(block, arm, variants[(block["family"], block["variant"])], key,
                       output, binary, environment, texts)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("probe", "run"):
        command = sub.add_parser(name)
        command.add_argument("--binary", type=Path, required=True)
        command.add_argument("--output", type=Path, required=True)
        command.add_argument("--model", default="haiku")
        if name == "run":
            command.add_argument("--binary-commit", required=True)
            command.add_argument("--seed", type=int, default=51701)
            command.add_argument("--host-label", default="claude-haiku")
    args = parser.parse_args()
    # Checked on every exit, a crash or an interrupt included: no token copy
    # may remain under HOMES and no output file may hold a credential.
    try:
        if args.command == "probe":
            binary = args.binary.resolve(strict=True)
            page = subprocess.check_output([binary, "docs", "agents"], text=True,
                                           env={"PATH": "/usr/bin", "LC_ALL": "C.UTF-8"})
            print(json.dumps(probe(binary, args.model, args.output.resolve(), page), indent=2))
        else:
            run(args)
    finally:
        verify_clean(args.output.resolve())


if __name__ == "__main__":
    main()
