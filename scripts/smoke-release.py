#!/usr/bin/env python3
"""Install the release package into an isolated prefix and smoke-test it in a clean root.

Usage:
  python3 scripts/smoke-release.py [ARCHIVE] [--out DIR]

ARCHIVE defaults to the one `scripts/package-release.py` wrote under
target/package. Every step runs under bubblewrap (required; the script
refuses to run without it): no network namespace, a cleared environment,
read-only /usr, a private /tmp, and none of the checkout, the home directory
or the Cargo and rustup homes visible. The probe checks that from inside.

1. Install. The extracted package is mounted read-only at /pkg. An unrelated
   `inv` program sits on PATH at /opt/other/bin/inv. `install.sh --prefix
   /opt/investigation --alias-inv` must install investigation, keep that
   `inv` byte for byte and still resolve `inv` to it. A second prefix whose
   bin/inv already exists must keep it too, and a third prefix, with no `inv`
   anywhere, gets the alias.
2. Smoke. Each command is a fresh process in an empty /work, with the prefix
   mounted read-only and PATH=/opt/other/bin:/opt/investigation/bin:/usr/bin:
   `version`; `init`; `start` with standard input attached; `record` of the
   transaction example that `schema question.create` prints, with the case ID
   filled in; `receipt` for both command IDs; `brief`; then `show` for every
   ID the receipts returned, each in its own process.

Writes DIR/smoke-transcript.json (every argv, exit code, standard input
digest and the standard output) and DIR/smoke-report.json (the archive,
binary and transcript digests, the platform and the isolation probe). DIR
defaults to target/package. Exits 1 if any check fails.
"""

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ACTOR = "agent:release-smoke"
UNRELATED_INV = b"#!/bin/sh\necho 'an unrelated inv program'\n"
QUESTION = "What held the connection?"
SOURCE_TEXT = b"The connection stayed checked out while the callback waited.\n"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


class CleanRoot:
    """bubblewrap with only /usr, the mounts named here, a private /tmp and no network."""

    def __init__(self, host):
        self.host = host

    def argv(self, binds, path, command):
        argv = ["bwrap", "--unshare-all", "--die-with-parent", "--new-session", "--hostname", "smoke",
                "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib",
                "--symlink", "usr/lib64", "/lib64", "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]
        for kind, source, destination in binds:
            argv += [kind, str(source), destination]
        argv += ["--clearenv", "--setenv", "PATH", path, "--setenv", "TMPDIR", "/tmp",
                 "--setenv", "LC_ALL", "C.UTF-8", "--chdir", "/work", "--"]
        return argv + command

    def run(self, binds, path, command, stdin=b""):
        result = subprocess.run(self.argv(binds, path, command), input=stdin, capture_output=True,
                                timeout=60, check=False)
        return result.returncode, result.stdout, result.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("archive", nargs="?", type=Path)
    parser.add_argument("--out", type=Path, default=ROOT / "target/package")
    args = parser.parse_args()
    archive = args.archive or next(iter(sorted((ROOT / "target/package").glob("investigation-*.tar.gz"))), None)
    if archive is None or not archive.is_file():
        print("smoke-release: no package archive; run scripts/package-release.py first", file=sys.stderr)
        return 2
    archive = archive.resolve()
    if not shutil.which("bwrap") or subprocess.run(
            ["bwrap", "--unshare-all", "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin",
             "--symlink", "usr/lib64", "/lib64", "--symlink", "usr/lib", "/lib", "true"],
            capture_output=True, check=False).returncode != 0:
        print("smoke-release: bubblewrap does not run here; the smoke test needs it", file=sys.stderr)
        return 2

    failures = []

    def check(condition, message):
        if not condition:
            failures.append(message)
        return condition

    host = Path(tempfile.mkdtemp(prefix="release-smoke-"))
    try:
        with tarfile.open(archive) as tar:
            tar.extractall(host / "extracted", filter="data")
        (package,) = list((host / "extracted").iterdir())
        for folder in ["work", "other/bin", "prefix", "prefix-kept/bin", "prefix-free"]:
            (host / folder).mkdir(parents=True)
        other_inv = host / "other/bin/inv"
        other_inv.write_bytes(UNRELATED_INV)
        other_inv.chmod(0o755)
        kept_inv = host / "prefix-kept/bin/inv"
        kept_inv.write_bytes(UNRELATED_INV)
        kept_inv.chmod(0o755)
        sandbox = CleanRoot(host)
        package_ro = ("--ro-bind", package, "/pkg")
        other_ro = ("--ro-bind", host / "other", "/opt/other")
        work = ("--bind", host / "work", "/work")
        on_path = "/opt/other/bin:/opt/investigation/bin:/usr/bin"

        # Isolation probe: nothing of the checkout or the host home is visible, and the network
        # namespace is not the host's.
        # The report names each forbidden path by its role, never by its host path.
        forbidden = {"checkout": str(ROOT), "home": str(Path.home()), "archive directory": str(archive.parent),
                     "cargo home": os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
                     "rustup home": os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup"))}
        script = " ".join(f"test -e '{path}' && echo 'VISIBLE:{role}';" for role, path in forbidden.items())
        _, out, _ = sandbox.run([work], "/usr/bin", ["sh", "-c", script + " readlink /proc/self/ns/net; "
                                                     "ls -A /work | wc -l"])
        lines = out.decode().splitlines()
        visible = [line[len("VISIBLE:"):] for line in lines if line.startswith("VISIBLE:")]
        probe = {"forbidden_checked": sorted(forbidden), "visible": visible,
                 "network_namespace_isolated": os.readlink("/proc/self/ns/net") not in lines,
                 "work_entries_at_start": int(lines[-1].strip()) if lines else None}
        check(not visible, f"host paths visible in the clean root: {visible}")
        check(probe["network_namespace_isolated"], "the clean root shares the host network namespace")
        check(probe["work_entries_at_start"] == 0, "the work directory was not empty")

        # 1. Install, three ways.
        installs = {}
        for label, prefix, alias_expected in [("inv on PATH", host / "prefix", False),
                                              ("inv in the prefix", host / "prefix-kept", False),
                                              ("no inv anywhere", host / "prefix-free", True)]:
            path = on_path if label == "inv on PATH" else "/opt/investigation/bin:/usr/bin"
            binds = [package_ro, ("--bind", prefix, "/opt/investigation"), work]
            if label == "inv on PATH":
                binds.append(other_ro)
            code, out, err = sandbox.run(binds, path, ["/pkg/install.sh", "--prefix", "/opt/investigation",
                                                       "--alias-inv"])
            _, resolved, _ = sandbox.run(binds, path, ["sh", "-c", "command -v inv; inv"])
            installed = prefix / "bin/investigation"
            alias = prefix / "bin/inv"
            installs[label] = {"exit": code, "output": out.decode(), "stderr": err.decode(),
                               "inv_resolves_to": resolved.decode().splitlines()}
            check(code == 0, f"install ({label}) exited {code}: {err.decode()}")
            check(installed.is_file() and installed.read_bytes() == (package / "bin/investigation").read_bytes(),
                  f"install ({label}) did not install the packaged binary")
            if alias_expected:
                check(alias.is_symlink() and os.readlink(alias) == "investigation",
                      f"install ({label}) did not add the inv alias")
            elif label == "inv on PATH":
                check(not alias.exists() and not alias.is_symlink(), "an inv alias shadowed the inv on PATH")
                check(other_inv.read_bytes() == UNRELATED_INV, "the unrelated inv on PATH changed")
                check(resolved.decode().splitlines() == ["/opt/other/bin/inv", "an unrelated inv program"],
                      f"inv no longer resolves to the unrelated program: {resolved.decode()!r}")
            else:
                check(not kept_inv.is_symlink() and kept_inv.read_bytes() == UNRELATED_INV,
                      "the existing inv in the prefix changed")
        # A second install into the same prefix refuses without --replace and changes nothing.
        before = (host / "prefix/bin/investigation").read_bytes()
        code, _, err = sandbox.run([package_ro, ("--bind", host / "prefix", "/opt/investigation"), work],
                                   "/usr/bin", ["/pkg/install.sh", "--prefix", "/opt/investigation"])
        installs["again without --replace"] = {"exit": code, "stderr": err.decode()}
        check(code != 0 and (host / "prefix/bin/investigation").read_bytes() == before,
              "a second install without --replace was not refused")

        # 2. Smoke: every command a fresh process in the clean root.
        binds = [("--ro-bind", host / "prefix", "/opt/investigation"), other_ro, work]
        transcript = []

        def call(argv, stdin=b"", expect=0):
            code, out, err = sandbox.run(binds, on_path, ["investigation", *argv], stdin)
            entry = {"argv": ["investigation", *argv], "exit": code, "stdin_sha256": sha256(stdin) if stdin else None,
                     "stdout": out.decode()}
            if err:
                entry["stderr"] = err.decode()
            transcript.append(entry)
            check(code == expect, f"{' '.join(argv[:4])} exited {code}: {out.decode()[:300]} {err.decode()[:300]}")
            try:
                return json.loads(out)
            except ValueError:
                check(False, f"{' '.join(argv[:4])} did not print one JSON object")
                return {}

        workspace = ["--workspace", "casework", "--actor", ACTOR]
        version = call(["version", "--format", "json"])
        call(["init", "casework", "--no-vault", "--format", "json"])
        started = call([*workspace, "start", QUESTION, "--attach", "-", "--command-id", "first-source",
                        "--format", "json"], SOURCE_TEXT)
        case_id = next((item["id"] for item in started.get("created", []) if item["client_ref"] == "case"), None)
        check(case_id is not None, "start returned no case ID")
        example = call(["--workspace", "casework", "schema", "question.create", "--format", "json"])
        request = dict(example.get("transaction_example", {}).get("input", {}))
        request.update({"case_id": case_id, "command_id": "smoke-question", "actor": ACTOR})
        recorded = call([*workspace, "record", "--input", "-", "--format", "json"], json.dumps(request).encode())
        check(recorded.get("ok") is True, "record was not accepted")
        receipts = [call([*workspace, "receipt", key, "--format", "json"]) for key in ["first-source",
                                                                                        "smoke-question"]]
        brief = call(["--workspace", "casework", "brief", "--format", "json"])
        check(brief.get("ok") is True and case_id in json.dumps(brief), "brief does not name the case")
        followed = {}
        for receipt in receipts:
            for item in receipt.get("created", []):
                shown = call(["--workspace", "casework", "--case", case_id, "show", item["id"], "--depth",
                              "summary", "--format", "json"])
                found = json.dumps(shown)
                followed[item["id"]] = check(shown.get("ok") is True and item["id"] in found,
                                             f"show {item['id']} did not return the record")
        check(len(followed) >= 6, f"the receipts returned only {len(followed)} IDs")
        check(any(key.startswith("q_") for key in followed), "no question ID was followed")

        args.out.mkdir(parents=True, exist_ok=True)
        transcript_bytes = json.dumps(transcript, indent=2, sort_keys=True).encode() + b"\n"
        (args.out / "smoke-transcript.json").write_bytes(transcript_bytes)
        binary = (package / "bin/investigation").read_bytes()
        report = {
            "archive": archive.name,
            "archive_sha256": sha256(archive.read_bytes()),
            "binary_sha256": sha256(binary),
            "package": json.loads((package / "PACKAGE.json").read_text()),
            "version": version.get("version"),
            "host": {"system": platform.system(), "release": platform.release(), "machine": platform.machine(),
                     "libc": " ".join(platform.libc_ver())},
            "isolation": {"mode": "bwrap", **probe},
            "installs": installs,
            "commands": len(transcript),
            "ids_followed": len(followed),
            "transcript": "smoke-transcript.json",
            "transcript_sha256": sha256(transcript_bytes),
            "failures": failures,
            "pass": not failures,
        }
        (args.out / "smoke-report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
        print(json.dumps({key: report[key] for key in ("archive", "archive_sha256", "binary_sha256", "commands",
                                                        "ids_followed", "transcript_sha256", "pass")}, indent=2))
        for failure in failures:
            print(f"FAILED: {failure}", file=sys.stderr)
        return 0 if not failures else 1
    finally:
        shutil.rmtree(host, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
