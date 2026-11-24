#!/usr/bin/env python3
"""Build the release candidate package for Linux x86_64, twice, and compare.

Usage:
  python3 scripts/package-release.py [--out DIR] [--single]
  python3 scripts/package-release.py notices [--manifest-path Cargo.toml]

The package is built from the committed tree of HEAD (`git archive`), never
from the working directory, so uncommitted changes are left out and the
report says whether there were any. Each build runs `cargo build --release
--locked --offline` for x86_64-unknown-linux-gnu with SOURCE_DATE_EPOCH set to
the commit time, every host path remapped (`--remap-path-prefix` for rustc,
`-ffile-prefix-map` for the bundled C) and its own target directory. The
second build uses a source copy at a different path. The binary and the
package archive of both builds must be byte-identical; a difference fails
the run and the report names the first differing offset.

The package `investigation-VERSION-x86_64-unknown-linux-gnu.tar.gz` holds
bin/investigation, LICENSE, NOTICES.md (third-party notices generated from
Cargo.lock offline: every crate linked into the binary with its license
expression and license files), PACKAGE.json (source commit and tree,
toolchain, platform, the glibc it needs), README.txt, install.sh and
SHA256SUMS over those files. Beside it the run writes SHA256SUMS for the
archive and package-report.json with both builds' digests.

Only Linux x86_64 is packaged; PACKAGE.json lists other platforms as not
packaged. Nothing is published or uploaded. No network is used: a crate not
in the local Cargo cache fails the offline build.
"""

import argparse
import gzip
import hashlib
import io
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"
PACKAGE = "investigation-cli"
BINARY = "investigation"
NOT_PACKAGED = {
    "aarch64-unknown-linux-gnu": "no build or test evidence on this architecture",
    "x86_64-unknown-linux-musl": "not built; the tested binary links glibc",
    "macOS": "not built or tested; the store's durability tests cover Linux filesystems only",
    "Windows": "not built or tested",
}
LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)", re.I)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def run(argv, **kwargs):
    return subprocess.run(argv, check=True, capture_output=True, text=True, **kwargs).stdout


def git(*args):
    return run(["git", *args], cwd=ROOT).strip()


# --- Third-party notices --------------------------------------------------------------------------


def linked_packages(manifest_path):
    """The packages the binary links: the normal-dependency closure of the CLI crate for the
    target platform, from `cargo metadata --locked --offline`."""
    metadata = json.loads(run(["cargo", "metadata", "--locked", "--offline", "--format-version", "1",
                               "--filter-platform", TARGET, "--manifest-path", str(manifest_path)]))
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    root = next(package["id"] for package in metadata["packages"] if package["name"] == PACKAGE)
    seen, stack = set(), [root]
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dependency in nodes[current]["deps"]:
            if any(kind["kind"] is None for kind in dependency["dep_kinds"]):
                stack.append(dependency["pkg"])
    return sorted((packages[i] for i in seen), key=lambda p: (p["name"], p["version"]))


def notices(manifest_path):
    """NOTICES.md: each third-party crate with its license expression and repository, then every
    distinct license file text once, with the crates that ship it."""
    third_party = [p for p in linked_packages(manifest_path) if p["source"] is not None]
    texts = {}
    rows = []
    for package in third_party:
        directory = Path(package["manifest_path"]).parent
        names = sorted(name for name in os.listdir(directory)
                       if LICENSE_FILE.match(name) and (directory / name).is_file())
        if not names:
            raise SystemExit(f"package-release: {package['name']} {package['version']} ships no license file")
        keys = []
        for name in names:
            text = (directory / name).read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n").strip()
            key = sha256(text.encode())[:16]
            texts.setdefault(key, {"text": text, "files": []})["files"].append(
                f"{package['name']} {package['version']}: {name}")
            keys.append(key)
        rows.append((package, keys))
    lines = [
        "# Third-party notices",
        "",
        f"`{BINARY}` is built from the {len(third_party)} third-party crates below: the normal dependency",
        f"graph of the command-line crate in Cargo.lock for {TARGET}, which is the linked code and the",
        "procedural macros that generate part of it. Build scripts' tools and test-only crates are not listed.",
        "Each crate's license expression is the one its manifest declares. `libsqlite3-sys` compiles the",
        "bundled SQLite amalgamation into the binary; SQLite itself is in the public domain.",
        "",
        "| Crate | Version | License | Repository | License texts |",
        "| --- | --- | --- | --- | --- |",
    ]
    for package, keys in rows:
        lines.append(f"| {package['name']} | {package['version']} | {package['license'] or 'not declared'} | "
                     f"{package.get('repository') or ''} | {', '.join(keys)} |")
    lines += ["", "## License texts", ""]
    for key in sorted(texts):
        entry = texts[key]
        lines += [f"### {key}", "", "Shipped as " + "; ".join(entry["files"]) + ".", "", "```text",
                  entry["text"].replace("```", "'''"), "```", ""]
    return "\n".join(lines)


# --- Build ----------------------------------------------------------------------------------------


def export_source(destination):
    """The committed tree of HEAD, extracted at `destination`."""
    destination.mkdir(parents=True)
    archive = subprocess.run(["git", "archive", "--format=tar", "HEAD"], cwd=ROOT, check=True,
                             capture_output=True).stdout
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(destination, filter="data")


def build(source, target_dir, epoch):
    home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).resolve()
    rustup = Path(os.environ.get("RUSTUP_HOME", Path.home() / ".rustup")).resolve()
    maps = [(source, "/build/investigation"), (target_dir, "/build/target"), (home, "/cargo"),
            (rustup, "/rustup")]
    environment = {key: value for key, value in os.environ.items()
                   if key in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "TMPDIR")}
    environment.update({
        "SOURCE_DATE_EPOCH": str(epoch),
        "CARGO_TARGET_DIR": str(target_dir),
        "CARGO_INCREMENTAL": "0",
        "RUSTFLAGS": " ".join(f"--remap-path-prefix={path}={to}" for path, to in maps),
        "CFLAGS": " ".join(f"-ffile-prefix-map={path}={to}" for path, to in maps),
        "LC_ALL": "C",
        "TZ": "UTC",
    })
    subprocess.run(["cargo", "build", "--release", "--locked", "--offline", "--target", TARGET,
                    "-p", PACKAGE, "--bin", BINARY], cwd=source, env=environment, check=True)
    return target_dir / TARGET / "release" / BINARY


def glibc_required(binary):
    """The newest GLIBC symbol version the binary references."""
    versions = {tuple(map(int, match.split(b".")))
                for match in re.findall(rb"GLIBC_(\d+\.\d+(?:\.\d+)?)\x00", binary.read_bytes())}
    return "GLIBC_" + ".".join(map(str, max(versions))) if versions else None


def toolchain(source):
    rustc = run(["rustc", "-vV"], cwd=source).strip().splitlines()
    cc = run(["cc", "--version"], cwd=source).splitlines()[0] if shutil.which("cc") else None
    return {"rustc": rustc, "cargo": run(["cargo", "-V"], cwd=source).strip(), "cc": cc,
            "python_zlib": __import__("zlib").ZLIB_VERSION}


def package(binary, source, epoch, out_dir, info):
    """The reproducible archive, written to `out_dir`; returns its path."""
    version = json.loads(run(["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version",
                              "1"], cwd=source))["packages"]
    version = next(p["version"] for p in version if p["name"] == PACKAGE)
    name = f"investigation-{version}-{TARGET}"
    files = {
        "bin/investigation": (binary.read_bytes(), 0o755),
        "LICENSE": ((source / "LICENSE").read_bytes(), 0o644),
        "NOTICES.md": (notices(source / "Cargo.toml").encode() + b"\n", 0o644),
        "install.sh": ((source / "scripts/release/install.sh").read_bytes(), 0o755),
        "README.txt": ((source / "scripts/release/README.txt").read_bytes(), 0o644),
    }
    info = {**info, "version": version, "target": TARGET, "glibc_required": glibc_required(binary),
            "binary_sha256": sha256(files["bin/investigation"][0])}
    files["PACKAGE.json"] = (json.dumps(info, indent=2, sort_keys=True).encode() + b"\n", 0o644)
    sums = "".join(f"{sha256(data)}  {path}\n" for path, (data, _) in sorted(files.items()))
    files["SHA256SUMS"] = (sums.encode(), 0o644)

    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        directories = sorted({"/".join(path.split("/")[:i]) for path in files
                              for i in range(1, path.count("/") + 1)})
        entries = [(d, None, 0o755) for d in [""] + directories] + \
                  [(path, data, mode) for path, (data, mode) in sorted(files.items())]
        for path, data, mode in entries:
            member = tarfile.TarInfo(f"{name}/{path}".rstrip("/"))
            member.mtime, member.mode, member.uid, member.gid = epoch, mode, 0, 0
            member.uname = member.gname = ""
            if data is None:
                member.type = tarfile.DIRTYPE
                tar.addfile(member)
            else:
                member.size = len(data)
                tar.addfile(member, io.BytesIO(data))
    archive = out_dir / f"{name}.tar.gz"
    out_dir.mkdir(parents=True, exist_ok=True)
    with open(archive, "wb") as handle, gzip.GzipFile(filename="", mode="wb", fileobj=handle,
                                                      mtime=epoch, compresslevel=9) as gz:
        gz.write(raw.getvalue())
    return archive, info


def first_difference(left, right):
    a, b = left.read_bytes(), right.read_bytes()
    for offset, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return offset
    return None if len(a) == len(b) else min(len(a), len(b))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", nargs="?", choices=["build", "notices"], default="build")
    parser.add_argument("--out", type=Path, default=ROOT / "target/package")
    parser.add_argument("--manifest-path", type=Path, default=ROOT / "Cargo.toml")
    parser.add_argument("--single", action="store_true",
                        help="build once and skip the reproducibility comparison")
    args = parser.parse_args()
    if args.command == "notices":
        sys.stdout.write(notices(args.manifest_path) + "\n")
        return 0
    if sys.platform != "linux" or platform.machine() != "x86_64":
        print(f"package-release: only {TARGET} is packaged; this host is {sys.platform} "
              f"{platform.machine()}", file=sys.stderr)
        return 2

    commit, tree = git("rev-parse", "HEAD"), git("rev-parse", "HEAD^{tree}")
    epoch = int(git("log", "-1", "--format=%ct", "HEAD"))
    dirty = bool(git("status", "--porcelain", "--untracked-files=no"))
    # On disk below target/, not in TMPDIR: two release target directories are large.
    (ROOT / "target").mkdir(exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix="package-scratch-", dir=ROOT / "target"))
    try:
        builds = []
        for label, relative in [("first", "a/src"), ("second", "b/elsewhere/src")][: 1 if args.single else 2]:
            source = scratch / relative
            export_source(source)
            binary = build(source, scratch / relative.split("/")[0] / "target", epoch)
            info = {"source_commit": commit, "source_tree": tree, "source_date_epoch": epoch,
                    "toolchain": toolchain(source), "platforms_not_packaged": NOT_PACKAGED,
                    "build": "cargo build --release --locked --offline --target "
                             f"{TARGET} -p {PACKAGE} --bin {BINARY}, host paths remapped"}
            archive, info = package(binary, source, epoch, scratch / label / "out", info)
            builds.append({"label": label, "binary": binary, "archive": archive, "info": info})

        first = builds[0]
        report = {
            "source_commit": commit,
            "source_tree": tree,
            "uncommitted_changes_left_out": dirty,
            "archive": first["archive"].name,
            "archive_sha256": sha256(first["archive"].read_bytes()),
            "binary_sha256": first["info"]["binary_sha256"],
            "glibc_required": first["info"]["glibc_required"],
            "toolchain": first["info"]["toolchain"],
            "builds": [{"label": b["label"], "binary_sha256": b["info"]["binary_sha256"],
                        "archive_sha256": sha256(b["archive"].read_bytes())} for b in builds],
        }
        reproducible = None
        if len(builds) == 2:
            binary_offset = first_difference(builds[0]["binary"], builds[1]["binary"])
            archive_offset = first_difference(builds[0]["archive"], builds[1]["archive"])
            reproducible = binary_offset is None and archive_offset is None
            report["reproducible"] = reproducible
            report["first_difference"] = {"binary": binary_offset, "archive": archive_offset}
        args.out.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(first["archive"], args.out / first["archive"].name)
        (args.out / "SHA256SUMS").write_text(f"{report['archive_sha256']}  {first['archive'].name}\n")
        (args.out / "package-report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
        print(json.dumps({key: report[key] for key in ("archive", "archive_sha256", "binary_sha256",
                                                        "source_commit", "uncommitted_changes_left_out")}
                         | {"reproducible": reproducible, "out": str(args.out)}, indent=2))
        return 0 if reproducible in (True, None) else 1
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
