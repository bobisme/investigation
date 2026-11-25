"""A fake of the viewer's command line interface, for testing scripts/obsidian-ui-check.py.

Never a real viewer. scripts/test-obsidian-ui-check.py installs a copy of this file as an
executable named `obsidian` in an empty folder and puts only that folder on PATH. It answers
the documented commands the adapter uses (https://obsidian.md/help/cli, checked 2026-11-24)
from the files of one vault on disk, and logs every argv it receives as one JSON line.

Environment:
  FAKE_VAULT            the vault folder it serves (required)
  FAKE_VAULT_NAME       the only vault= name it accepts (default: the folder's name)
  FAKE_STATE            folder for argv.jsonl and the active view (required)
  FAKE_VERSION          what `version` prints (default "1.15.0 (installer 1.12.7)")
  FAKE_VERSION_FAIL=1   `version` fails, as when the app cannot be reached
Faults, each a way the real viewer could disagree with the static files:
  FAKE_DROP_ROW=PATH    base:query of that Base drops its last row
  FAKE_EXTRA_UNRESOLVED=SOURCE|LINK   one more unresolved link, from SOURCE
  FAKE_HIDE=PATH[:PATH] the viewer does not know these files (files, file, open)
  FAKE_IGNORE_CONFIG=1  new links ignore app.json (setup not honoured)
  FAKE_COMMUNITY=ID     a community plugin is enabled
  FAKE_UNRESOLVED_SHAPE=real|list|map   JSON shape of `unresolved verbose format=json`;
                        real (default) is what Obsidian 1.15.1 printed: link, count and
                        sources as strings (several sources joined by ", " is assumed)
  FAKE_STALE_UNRESOLVED=SOURCE|LINK   a stale cache: listed as unresolved until
                        FAKE_STALE_RELOADS reloads (default 1) have happened and
                        FAKE_STALE_POLLS answers (default 1) have been given since the last
  FAKE_UNRESOLVED_FLAP=1  every unresolved answer differs from the one before
"""

import json
import os
from pathlib import Path
import re
import sys

VAULT = Path(os.environ["FAKE_VAULT"])
NAME = os.environ.get("FAKE_VAULT_NAME", VAULT.name)
STATE = Path(os.environ["FAKE_STATE"])
HIDDEN = set(filter(None, os.environ.get("FAKE_HIDE", "").split(":")))


def fail(message):
    print(f"Error: {message}")
    sys.exit(1)


def files():
    found = []
    for path in sorted(VAULT.rglob("*")):
        relative = path.relative_to(VAULT)
        if path.is_file() and not any(part.startswith(".") for part in relative.parts):
            if relative.as_posix() not in HIDDEN:
                found.append(relative.as_posix())
    return found


def known(path):
    if path not in files():
        fail(f'File "{path}" not found.')
    return VAULT / path


def frontmatter(text):
    values, key = {}, None
    if not text.startswith("---\n"):
        return values
    for line in text[4:text.find("\n---\n", 4)].split("\n"):
        if line.startswith("  - ") and key:
            values.setdefault(key, []).append(json.loads(line[4:]) if line[4:].startswith('"')
                                              else line[4:])
            continue
        match = re.match(r"^([A-Za-z0-9_.]+):(?: (.*))?$", line)
        if match:
            key, raw = match.groups()
            if raw is None or raw == "[]":
                values[key] = []
            elif raw.startswith('"'):
                values[key] = json.loads(raw)
            elif re.fullmatch(r"-?\d+", raw):
                values[key] = int(raw)
            else:
                values[key] = raw
    return values


def parse_filter(lines, start, indent):
    """A list at `indent` of statements and one-key groups, as the renderer writes them."""
    items, index = [], start
    while index < len(lines):
        line = lines[index]
        depth = len(line) - len(line.lstrip())
        if depth < indent or not line.strip():
            break
        body = line.strip()[2:]
        if body.endswith(":"):
            members, index = parse_filter(lines, index + 1, depth + 4)
            items.append({body[:-1]: members})
        else:
            items.append(json.loads(body))
            index += 1
    return items, index


def base_filter(text):
    lines = text.split("\n")
    start = lines.index("filters:")
    group = lines[start + 1].strip()[:-1]
    members, _ = parse_filter(lines, start + 2, 4)
    return {group: members}


def holds(node, front):
    if isinstance(node, str):
        prop, op, value = node.split(" ", 2)
        actual = front.get(prop)
        if op == "==":
            return actual == json.loads(value)
        if op == "!=":
            return actual != json.loads(value)
        if op == ">":
            return isinstance(actual, int) and actual > int(value)
        fail(f"unknown operator {op}")
    (key, members), = node.items()
    results = [holds(member, front) for member in members]
    return {"and": all, "or": any}[key](results) if key != "not" else not any(results)


def wikilinks(text):
    for match in re.finditer(r"\[\[([^\]]+?)\]\]", text):
        yield match.group(1).replace("\\|", "|").split("|", 1)[0].split("#", 1)[0]


def resolves(target, everything):
    if target in everything or target + ".md" in everything:
        return True
    return any(Path(path).stem == target for path in everything)


def unresolved():
    everything = files()
    found = {}
    for path in everything:
        if path.endswith(".md"):
            for target in wikilinks((VAULT / path).read_text(encoding="utf-8")):
                if not resolves(target, everything):
                    found.setdefault(target, []).append(path)
    extra = os.environ.get("FAKE_EXTRA_UNRESOLVED")
    if extra:
        source, link = extra.split("|", 1)
        found.setdefault(link, []).append(source)
    reloads, polls = counter("reloads"), counter("polls-since-reload")
    bump("polls-since-reload")
    stale = os.environ.get("FAKE_STALE_UNRESOLVED")
    if stale and (reloads < int(os.environ.get("FAKE_STALE_RELOADS", "1"))
                  or polls < int(os.environ.get("FAKE_STALE_POLLS", "1"))):
        source, link = stale.split("|", 1)
        found.setdefault(link, []).append(source)
    if os.environ.get("FAKE_UNRESOLVED_FLAP"):
        found.setdefault(f"flap {counter('flap')}", []).append("Notes/Owner note.md")
        bump("flap")
    shape = os.environ.get("FAKE_UNRESOLVED_SHAPE", "real")
    if shape == "map":
        return found
    if shape == "list":
        return [{"link": link, "sources": sources} for link, sources in found.items()]
    return [{"link": link, "count": str(len(sources)), "sources": ", ".join(sources)}
            for link, sources in found.items()]


def counter(name):
    path = STATE / f"count-{name}"
    return int(path.read_text()) if path.exists() else 0


def bump(name, reset=False):
    (STATE / f"count-{name}").write_text("0" if reset else str(counter(name) + 1))


def evaluate(code):
    if code.startswith("/*inv-ui:active-view*/"):
        active = STATE / "active.json"
        return active.read_text() if active.exists() else json.dumps({"type": None, "path": None})
    if code.startswith("/*inv-ui:blocks*/"):
        path = json.loads(re.search(r'getAbstractFileByPath\(("(?:[^"\\]|\\.)*")\)', code)[1])
        if path not in files():
            return "null"
        text = (VAULT / path).read_text(encoding="utf-8")
        return json.dumps(re.findall(r" \^([A-Za-z0-9-]+)$", text, re.MULTILINE))
    if code.startswith("/*inv-ui:link*/"):
        path = json.loads(re.search(r'getAbstractFileByPath\(("(?:[^"\\]|\\.)*")\)', code)[1])
        if path not in files():
            return "null"
        config = {}
        if not os.environ.get("FAKE_IGNORE_CONFIG"):
            try:
                config = json.loads((VAULT / ".obsidian/app.json").read_text())
            except (OSError, ValueError):
                config = {}
        target = path[:-3] if config.get("newLinkFormat") == "absolute" else Path(path).stem
        if config.get("useMarkdownLinks", False):
            return json.dumps(f"[{Path(path).stem}]({path.replace(' ', '%20')})")
        return json.dumps(f"[[{target}]]")
    fail("eval is only answered for the adapter's marked snippets")


def main():
    arguments = sys.argv[1:]
    STATE.mkdir(parents=True, exist_ok=True)
    with open(STATE / "argv.jsonl", "a", encoding="utf-8") as log:
        log.write(json.dumps(arguments) + "\n")
    if arguments == ["version"]:
        if os.environ.get("FAKE_VERSION_FAIL"):
            fail("Unable to connect to main process")
        print(os.environ.get("FAKE_VERSION", "1.15.0 (installer 1.12.7)"))
        return
    if not arguments or not arguments[0].startswith("vault="):
        fail("this fake answers only commands that name vault= first")
    if arguments[0] != f"vault={NAME}":
        fail(f'Vault "{arguments[0][6:]}" not found.')
    command, rest = arguments[1], arguments[2:]
    params = dict(item.split("=", 1) for item in rest if "=" in item)
    flags = {item for item in rest if "=" not in item}
    if command == "vault":
        print(VAULT if params.get("info") == "path" else f"name\t{NAME}\npath\t{VAULT}")
    elif command == "plugins:enabled":
        community = os.environ.get("FAKE_COMMUNITY")
        print(json.dumps([{"id": community}] if community else []))
    elif command == "plugins:restrict":
        print("Restricted mode: on")
    elif command == "files":
        listed = [p for p in files() if "ext" not in params or p.endswith("." + params["ext"])]
        print(len(listed) if "total" in flags else "\n".join(listed))
    elif command == "file":
        path = known(params["path"])
        print(f"path\t{params['path']}\nname\t{path.stem}\nextension\t{path.suffix[1:]}\n"
              f"size\t{path.stat().st_size}")
    elif command == "open":
        path = known(params["path"])
        kind = {".md": "markdown", ".canvas": "canvas", ".base": "bases"}[path.suffix]
        (STATE / "active.json").write_text(json.dumps({"type": kind, "path": params["path"]}))
    elif command == "aliases":
        front = frontmatter(known(params["path"]).read_text(encoding="utf-8"))
        print("\n".join(front.get("aliases") or []))
    elif command == "base:query":
        if params.get("format") != "paths":
            fail("this fake answers format=paths only")
        base = known(params["path"]).read_text(encoding="utf-8")
        if f'name: "{params.get("view")}"' not in base:
            fail(f'View "{params.get("view")}" not found.')
        condition = base_filter(base)
        rows = [p for p in files() if p.endswith(".md")
                and holds(condition, frontmatter((VAULT / p).read_text(encoding="utf-8")))]
        if os.environ.get("FAKE_DROP_ROW") == params["path"] and rows:
            rows.pop()
        print("\n".join(rows))
    elif command == "unresolved":
        print(json.dumps(unresolved()))
    elif command == "eval":
        print(evaluate(params["code"]))
    elif command in ("reload", "restart"):
        bump("reloads")
        bump("polls-since-reload", reset=True)
        print("")
    elif command == "dev:errors":
        print("No errors captured.")
    elif command == "search":
        query = params["query"]
        print(json.dumps([p for p in files() if p.endswith(".md")
                          and query in (VAULT / p).read_text(encoding="utf-8")]))
    else:
        fail(f"unknown command {command}")


main()
