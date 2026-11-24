#!/usr/bin/env python3
"""Scan the default surfaces of one executable for named tools and security concepts.

    python3 scripts/check-neutrality.py                       # builds target/debug/investigation
    python3 scripts/check-neutrality.py target/debug/investigation --json

Plan §1.1 (Domain) and §20.1 X3: nothing in default help, the on-disk default workspace,
the schemas or the binary requires a named external tool, target, permission, principal
or security concept. This is the neutrality step of the Milestone B proof
(scripts/prove_milestone_b.py), which loads this file; `just check` runs it alone so a
regression is seen before the next proof run. It is broader than check-boundaries.py,
whose reviewed deny-list it includes: it adds a list of named tools and security terms,
the help of every subcommand, the output of `schema` for every operation, and the
`onboard` text of a fresh workspace.

Every match is reported. A match blocks when it is in default help, the published
schemas or the fresh workspace, unless an entry below allows it. Each entry names the
plan text that allows it, and allowed matches stay in the report.
"""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
# Named external tools and security concepts that the default surfaces must not contain,
# beyond the reviewed deny-list in scripts/neutrality-denylist.txt.
NAMED_TOOLS = (
    "obsidian", "jira", "github", "gitlab", "bitbucket", "asana", "trello",
    "docker", "kubernetes", "pytest", "valgrind", "wireshark", "prometheus", "grafana", "datadog",
    "sentry", "jaeger", "jmeter", "locust", "burp", "nmap", "metasploit", "semgrep", "codeql",
    "sonarqube", "openai", "chatgpt", "claude", "ollama", "copilot",
)
SECURITY_TERMS = (
    "cve", "cwe", "cvss", "owasp", "exploit", "vulnerability", "vulnerabilities", "attack",
    "attacker", "threat model", "malware", "pentest", "penetration test", "privilege escalation",
    "xss", "csrf", "sql injection", "red team", "security", "credential",
)
# Surfaces where a match blocks. Subcommand help is scanned and reported only.
REQUIRED = ("default help", "published schemas", "fresh workspace")
# A named tool that one line of one surface may name, with the plan text that allows it.
ALLOWED_NAMED = {
    "obsidian": {
        "surface": "default help",
        "line": r"^obsidian\s{2,}",
        "reason": "The plan's command catalog (§9.1) names `obsidian setup` as an optional review "
                  "integration that touches only named owned files, and §13.3 step 1 is "
                  "`inv obsidian setup --vault PATH`. The subcommand is optional and acts only on a "
                  "vault the user names, so it does not make the viewer a requirement (X3).",
    },
}
# A phrase whose words are not a security concept in the plan's sense, with the plan text.
# Only the phrase is exempt: the same word anywhere else on the line still matches.
ALLOWED_PHRASES = {
    r"completeness(?: and |/)security guarantees": {
        "terms": ("security",),
        "reason": "The plan (§15.4) says a review vault and a recovery archive have different "
                  "completeness/security guarantees: a property of the tool's own export products "
                  "(what an export may disclose, §16.3), not a security domain, practice or "
                  "target that a case must adopt (§1.1 Domain).",
    },
}


def patterns():
    denylist = [line.strip() for line in (ROOT / "scripts/neutrality-denylist.txt").read_text().splitlines()
                if line.strip() and not line.startswith("#")]
    categories = {"deny-list": denylist, "named tool": NAMED_TOOLS, "security concept": SECURITY_TERMS}
    compiled = [(category, term, re.compile(rf"\b{re.escape(term)}\b", re.IGNORECASE))
                for category, terms in categories.items() for term in terms]
    return {category: len(terms) for category, terms in categories.items()}, compiled


def surfaces(cli, src, work):
    """Every default surface, as {surface: {document: text}}, plus doctor and method list.

    `cli(*args)` runs the executable in `work` and returns its stdout, failing on a nonzero
    exit; `src` is the tree whose schemas are scanned."""
    found = {}
    help_text = cli("--help")
    found["default help"] = {"investigation --help": help_text}
    commands = re.findall(r"(?m)^  ([a-z][a-z-]*)\s{2,}", help_text.split("Commands:", 1)[1].split("Options:", 1)[0])
    found["subcommand help"] = {f"investigation {name} --help": cli(name, "--help")
                                for name in commands if name != "help"}
    listed = json.loads(cli("schema", "--list", "--format", "json"))
    operations = [name for stage in listed["stages"] for name in stage["operations"]]
    published = {"investigation schema --list": json.dumps(listed)}
    for name in operations:
        published[f"investigation schema {name}"] = cli("schema", name, "--format", "json")
    for path in sorted((src / "schemas").rglob("*")):
        if path.is_file():
            published[str(path.relative_to(src))] = path.read_text(encoding="utf-8")
    found["published schemas"] = published
    cli("init", "fresh")
    fresh = work / "fresh"
    files = {f"fresh workspace/{path.relative_to(fresh)}": path.read_bytes().decode("utf-8", "replace")
             for path in sorted(fresh.rglob("*")) if path.is_file()}
    files["investigation onboard"] = cli("--workspace", fresh, "onboard", "--format", "json")
    found["fresh workspace"] = files
    doctor = json.loads(cli("--workspace", fresh, "doctor", "--format", "json"))
    packs = json.loads(cli("--workspace", fresh, "method", "list", "--format", "json"))
    return found, doctor, packs


def allowance(surface, line, category, term, pattern):
    """The reason a match on this line is allowed, or None when it blocks."""
    entry = ALLOWED_NAMED.get(term) if category == "named tool" else None
    if entry and entry["surface"] == surface and re.search(entry["line"], line.strip()):
        return entry["reason"]
    for phrase, allowed in ALLOWED_PHRASES.items():
        if term in allowed["terms"]:
            masked = re.sub(phrase, lambda found: " " * len(found.group()), line, flags=re.IGNORECASE)
            if masked != line and not pattern.search(masked):
                return allowed["reason"]
    return None


def hits(found, compiled):
    """Every match, each with the reason it is allowed when an entry allows it."""
    result = {}
    for surface, documents in found.items():
        for document, content in documents.items():
            for number, line in enumerate(content.splitlines(), start=1):
                for category, term, pattern in compiled:
                    if pattern.search(line):
                        hit = {"category": category, "term": term, "where": f"{document}:{number}",
                               "line": line.strip()[:160]}
                        reason = allowance(surface, line, category, term, pattern)
                        if reason:
                            hit["allowed"] = reason
                        result.setdefault(surface, []).append(hit)
    return result


def scan(cli, src, work):
    """The neutrality report: every match, the allowed ones with their reasons, and the blocking ones."""
    terms, compiled = patterns()
    found, doctor, packs = surfaces(cli, src, work)
    matched = hits(found, compiled)
    blocking = [hit for surface in REQUIRED for hit in matched.get(surface, []) if "allowed" not in hit]
    allowed = [{"surface": surface, **hit} for surface in REQUIRED for hit in matched.get(surface, [])
               if "allowed" in hit]
    built_in = [pack["pack_id"] for pack in doctor["capabilities"]["method_packs"]["built_in"]]
    detail = {
        "surfaces_scanned": {surface: len(documents) for surface, documents in found.items()},
        "terms": terms, "hits": matched, "allowed": allowed, "blocking": blocking,
        "built_in_packs": built_in, "method_list": packs, "versions": doctor["versions"],
    }
    return detail


def self_test():
    """The allowances are as narrow as stated: the same words elsewhere still block."""
    _, compiled = patterns()
    cases = {
        ("default help", "  obsidian       Preview viewer settings"): False,
        ("default help", "  render  Write an Obsidian vault"): True,
        ("published schemas", "  obsidian       Preview viewer settings"): True,
        ("published schemas", "different completeness and security guarantees (§16.3)"): False,
        ("published schemas", "completeness/security guarantees"): False,
        ("published schemas", "completeness and security guarantees for a security review"): True,
        ("published schemas", "the security guarantees of a target"): True,
        ("fresh workspace", "a security finding"): True,
    }
    for (surface, line), blocks in cases.items():
        found = hits({surface: {"line": line}}, compiled).get(surface, [])
        blocked = any("allowed" not in hit for hit in found)
        assert blocked == blocks, (surface, line, found)


def built_binary():
    child = subprocess.run(["cargo", "build", "--locked", "--quiet", "-p", "investigation-cli",
                            "--message-format=json"], cwd=ROOT, capture_output=True, text=True)
    if child.returncode:
        raise SystemExit(child.stderr)
    for line in child.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "investigation" \
                and message.get("executable"):
            return Path(message["executable"])
    raise SystemExit("cargo built no investigation executable")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    parser.add_argument("binary", type=Path, nargs="?", help="Default: build target/debug/investigation.")
    parser.add_argument("--json", action="store_true", help="Print the whole report as JSON.")
    args = parser.parse_args()
    self_test()
    binary = (args.binary or built_binary()).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="neutrality-") as directory:
        work = Path(directory)
        (work / "home").mkdir()
        env = {"PATH": "", "HOME": str(work / "home"), "TMPDIR": directory, "LC_ALL": "C.UTF-8", "TZ": "UTC"}

        def cli(*command):
            child = subprocess.run([binary, *map(str, command)], cwd=work, env=env, capture_output=True, text=True)
            if child.returncode:
                raise SystemExit(f"investigation {' '.join(map(str, command))}: exit {child.returncode}\n"
                                 f"{child.stdout}{child.stderr}")
            return child.stdout

        detail = scan(cli, ROOT, work)
    if args.json:
        print(json.dumps(detail, indent=2, ensure_ascii=False))
    failed = False
    for hit in detail["blocking"]:
        print(f"{hit['where']}: default surface contains {hit['category']} {hit['term']!r}: {hit['line']}",
              file=sys.stderr)
        failed = True
    if detail["built_in_packs"] != ["general"]:
        print(f"built-in method packs are {detail['built_in_packs']}, not only 'general'", file=sys.stderr)
        failed = True
    if not args.json:
        scanned = sum(detail["surfaces_scanned"].values())
        print(f"neutrality: {scanned} documents, {sum(detail['terms'].values())} terms, "
              f"{len(detail['blocking'])} blocking, {len(detail['allowed'])} allowed "
              f"({', '.join(sorted({hit['term'] for hit in detail['allowed']})) or 'none'})")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
