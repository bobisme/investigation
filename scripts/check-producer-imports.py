"""Check that the published core imports, links and invokes no named producer (plan §20.7).

The shipped crates are the libraries a producer links (core, store, recording), the
renderer and the CLI. This check fails when any of them:

1. has a normal or build dependency, at any depth, on a workspace producer crate
   (the example producers) or on a crate whose name contains a named tool;
2. names a producer crate anywhere in its sources, or a named tool in a `use` or
   `extern crate` line;
3. names a process or network-socket API outside a string literal in its sources;
   the core records what producers report and never runs them (threads are allowed);
4. carries a reference project named in the neutrality deny-list in the built executable.

The named tools are the reviewed deny-list (scripts/neutrality-denylist.txt, first
section) plus the named-tool list of the neutrality scan (scripts/check-neutrality.py,
the Milestone B proof's neutrality step). A generic producer is
shown to work by the conformance suite (scripts/run-conformance.py), not here.

    python3 scripts/check-producer-imports.py target/debug/investigation [--json]
"""

import argparse
import importlib.util
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SHIPPED = ("investigation-core", "investigation-store", "investigation-recording",
           "investigation-render", "investigation-cli")
PROCESS_OR_NETWORK = re.compile(
    r"std::process::(Command|Stdio)|process::Command|std::net::|TcpStream|UdpSocket"
    r"|UnixStream|libc::(exec|fork|posix_spawn)")


def named_tools():
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("neutrality", ROOT / "scripts" / "check-neutrality.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    reviewed = []
    for line in (ROOT / "scripts" / "neutrality-denylist.txt").read_text().splitlines():
        if line.startswith("#") and reviewed:
            break  # the first section names reference projects; later ones are concepts
        if line.strip() and not line.startswith("#"):
            reviewed.append(line.strip().lower())
    return sorted(set(reviewed) | {tool.lower() for tool in module.NAMED_TOOLS}), reviewed


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("binary", type=Path)
    parser.add_argument("--json", action="store_true", help="print the report as JSON")
    args = parser.parse_args()

    tools, reviewed = named_tools()
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"], cwd=ROOT, text=True))
    packages = {package["id"]: package for package in metadata["packages"]}
    members = {packages[member]["name"] for member in metadata["workspace_members"]}
    producers = sorted(name for name in members if name not in SHIPPED
                       and name not in {"investigation-bench", "investigation-mcp"})
    graph = {node["id"]: [dep["pkg"] for dep in node["deps"]
                          if any(kind["kind"] != "dev" for kind in dep["dep_kinds"])]
             for node in metadata["resolve"]["nodes"]}

    failures, closure = [], set()
    for root in (pid for pid, package in packages.items() if package["name"] in SHIPPED):
        seen, pending = {root}, [root]
        while pending:
            for dependency in graph.get(pending.pop(), []):
                if dependency not in seen:
                    seen.add(dependency)
                    pending.append(dependency)
        closure |= seen
    linked = sorted({packages[pid]["name"] for pid in closure})
    for name in linked:
        if name in producers:
            failures.append(f"dependency: a shipped crate links producer crate {name}")
        for tool in tools:
            if tool.replace(" ", "-") in name.lower() or tool.replace(" ", "_") in name.lower():
                failures.append(f"dependency: linked crate {name} names {tool!r}")

    import_pattern = re.compile(r"^\s*(pub\s+)?(use|extern\s+crate)\b.*$")
    producer_idents = [name.replace("-", "_") for name in producers]
    files = 0
    for crate in SHIPPED:
        for path in sorted((ROOT / "crates" / crate / "src").rglob("*.rs")):
            files += 1
            relative = path.relative_to(ROOT)
            for number, line in enumerate(path.read_text().splitlines(), 1):
                lowered = line.lower()
                for ident in producer_idents:
                    if re.search(rf"\b{ident}\b", line):
                        failures.append(f"{relative}:{number}: names producer crate {ident}")
                if import_pattern.match(line):
                    for tool in tools:
                        if re.search(rf"\b{re.escape(tool.replace(' ', '_'))}\b", lowered):
                            failures.append(f"{relative}:{number}: imports a path naming {tool!r}")
                if PROCESS_OR_NETWORK.search(re.sub(r'"(\\.|[^"\\])*"', '""', line)):
                    failures.append(f"{relative}:{number}: starts a process or opens a socket")

    data = args.binary.read_bytes().lower()
    for tool in reviewed:
        if tool.encode() in data:
            failures.append(f"{args.binary.name}: the executable contains deny-listed name {tool!r}")

    report = {"shipped_crates": list(SHIPPED), "producer_crates": producers, "linked_packages": len(linked),
              "source_files": files, "named_tools": len(tools), "binary_scanned_for": reviewed,
              "failures": failures, "passed": not failures}
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        for failure in failures:
            print(failure)
        print(f"{'passed' if not failures else 'FAILED'}: {len(SHIPPED)} shipped crates, {len(linked)} linked "
              f"packages, {files} source files, {len(tools)} named tools; producer crates {producers}")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
