#!/usr/bin/env python3
"""Check, seal, verify and open the sealed release-trial case families (W14.01).

The sealed families' source bytes, objectives and grading keys never appear in
the checkout as plain text. The checkout holds only:

* opaque registrations in fixtures/family-registry.json (split ``sealed``,
  per-variant digests of objective, source and update bytes, key locator);
* training/release/families/manifest.json, which commits the digest of every
  key, objective and source file, the plaintext bundle digest and the
  preregistered counts it was checked against;
* training/release/families/sealed.tar.gz.enc, the bundle encrypted with a
  passphrase kept outside the repository.

``verify`` without a passphrase checks counts, registry/manifest agreement and
the ciphertext digest. With ``--key-file`` it also decrypts into a private
temporary directory and checks every committed digest and rubric.
"""

import argparse
from collections import Counter
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import subprocess
import sys
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
REGISTRY = FIXTURES / "family-registry.json"
RELEASE = ROOT / "training/release/families"
MANIFEST = RELEASE / "manifest.json"
CIPHERTEXT = RELEASE / "sealed.tar.gz.enc"
PREREGISTRATION = "docs/release-trial.md"
PREREGISTRATION_COMMIT = "214c45b"
DEFAULT_KEY = Path("~/.local/share/investigation-evaluator/release-families.key")
DOMAINS = ("debugging", "measurement", "source-research", "architecture")
TWINS = ("missing", "cosmetic")
ITEMS = ("current_disposition", "change_since_review", "decisive_citation",
         "material_limitation", "discriminating_followup")
FIELDS = ("answer", "citation", "limitation", "next")
HEDGES = ("\\b(?:provisional|conditional|inconclusive|hypothetical|unsupported|unwarranted|"
          "unverified|insufficient|unknown|pending|before|prior|previous|earlier|would|could|"
          "might|if|not|never|cannot|fails?|ineligible|unless|until)\\b")
EARLIER_STATE = ("At review the leading candidate looked likely but was pending, and one input "
                 "was confounded and unverified.")
FAMILY_ID = re.compile(r"^release-(debugging|measurement|source-research|architecture)-(\d{2})$")
SHA = re.compile(r"^[a-f0-9]{64}$")
OPENSSL = ["openssl", "enc", "-aes-256-cbc", "-pbkdf2", "-iter", "600000", "-md", "sha256"]
ALLOWED_RELEASE_FILES = {"README.md", "manifest.json", "sealed.tar.gz.enc"}

GRADER = runpy.run_path(str(ROOT / "scripts/grade-a-gate.py"))
REGISTRY_CHECK = runpy.run_path(str(ROOT / "scripts/check-fixture-families.py"))


def load(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def dump(value):
    return json.dumps(value, indent=2, ensure_ascii=False) + "\n"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def text_bytes(lines):
    return ("\n".join(lines) + "\n").encode("utf-8")


# --- Preregistered counts ----------------------------------------------------------

def preregistered_counts():
    """Read the counts from the committed preregistration, not from a constant."""
    try:
        text = subprocess.run(["git", "-C", str(ROOT), "show", f"{PREREGISTRATION_COMMIT}:{PREREGISTRATION}"],
                              check=True, capture_output=True, text=True).stdout
    except (OSError, subprocess.CalledProcessError):
        text = (ROOT / PREREGISTRATION).read_text(encoding="utf-8")
    families = re.search(r"Enroll \*\*(\d+) independent families\*\*, (\d+) each for", text)
    assignments = re.search(r"contains ([\d,]+) paired-arm assignments", text)
    if not families or not assignments or "an original variant and a prespecified missing-evidence or cosmetic twin" not in text:
        raise ValueError("Cannot read the preregistered family counts")
    return {
        "source": f"{PREREGISTRATION}@{PREREGISTRATION_COMMIT}",
        "families": int(families.group(1)),
        "per_domain": int(families.group(2)),
        "variants_per_family": 2,
        "paired_arm_assignments": int(assignments.group(1).replace(",", "")),
        "quote": families.group(0) + " intermittent debugging, p99 regression measurement, "
                 "source research and architecture choice. Each family has an original variant and "
                 "a prespecified missing-evidence or cosmetic twin.",
    }


def check_counts(families):
    """families: list of (family_id, domain, twin)."""
    counts = preregistered_counts()
    problems = []
    if len(families) != counts["families"]:
        problems.append(f"{len(families)} families, preregistered {counts['families']}")
    by_domain = Counter(domain for _, domain, _ in families)
    for domain in DOMAINS:
        if by_domain[domain] != counts["per_domain"]:
            problems.append(f"{domain}: {by_domain[domain]} families, preregistered {counts['per_domain']}")
        twins = Counter(twin for _, d, twin in families if d == domain)
        if abs(twins["missing"] - twins["cosmetic"]) > 1:
            problems.append(f"{domain}: twin types not balanced {dict(twins)}")
    if set(by_domain) - set(DOMAINS):
        problems.append(f"unexpected domains {sorted(set(by_domain) - set(DOMAINS))}")
    # 2 variants x 2 arms x 2 models per family.
    if len(families) * 2 * 2 * 2 != counts["paired_arm_assignments"]:
        problems.append("family count does not reproduce the preregistered assignment total")
    if problems:
        raise ValueError("Sealed set does not match the preregistration: " + "; ".join(problems))
    return counts


# --- Authoring specification checks -------------------------------------------------

def expand(clauses):
    out = []
    for item in clauses:
        item = dict(item)
        if item.get("unless") == "hedges":
            item["unless"] = [HEDGES]
        out.append(item)
    return out


def rubric(spec, variant):
    data = spec["variants"][variant]
    return {"warranted": expand(data["warranted"]), "false_promotions": expand(data["false_promotions"])}


def documents(spec, variant):
    return {"source.txt": text_bytes(spec["source"]), "update.txt": text_bytes(spec["variants"][variant]["update"])}


def grade(answer, rubric_, docs):
    row = {
        "id": "reference",
        "answer": {field: GRADER["clean_field"](answer[field]) for field in ("answer", "limitation", "next")},
        "citations": GRADER["select_citations"](answer["citation"], docs, {}),
    }
    return GRADER["score"](row, rubric_)


def check_answer(answer, where):
    if not isinstance(answer, dict) or set(answer) != set(FIELDS) or not all(
            isinstance(answer[f], str) and answer[f].strip() for f in FIELDS):
        raise ValueError(f"{where}: an answer needs exactly the non-empty fields {FIELDS}")


def check_lines(lines, where, low, high):
    if not isinstance(lines, list) or not low <= len(lines) <= high:
        raise ValueError(f"{where}: needs {low}-{high} lines")
    for line in lines:
        if not isinstance(line, str) or not line.strip() or len(line) > 260 or "\n" in line:
            raise ValueError(f"{where}: each line is one non-empty sentence group of at most 260 characters")
        if not line.isascii():
            raise ValueError(f"{where}: use ASCII text only")


def check_spec(spec, name=None):
    """Validate one authored family. Returns a list of soft notes."""
    family = spec.get("family", "")
    match = FAMILY_ID.fullmatch(family)
    if not match or (name and name != f"{family}.json"):
        raise ValueError(f"{name}: family must be release-<domain>-NN and match the file name")
    if spec.get("domain") != match.group(1):
        raise ValueError(f"{family}: domain must match the family ID")
    twin = spec.get("twin")
    if twin not in TWINS or set(spec.get("variants", {})) != {"original", twin}:
        raise ValueError(f"{family}: variants must be exactly original and the {twin!r} twin")
    for field in ("title", "situation", "objective", "handoff"):
        if not isinstance(spec.get(field), str) or not spec[field].strip() or "\n" in spec[field]:
            raise ValueError(f"{family}: {field} must be a one-line string")
    if len(spec["objective"]) > 400:
        raise ValueError(f"{family}: objective is too long")
    check_lines(spec["source"], f"{family}/source", 6, 14)
    rivals = spec.get("rivals")
    if not isinstance(rivals, list) or len(rivals) < 2 or not all(
            isinstance(r, dict) and set(r) == {"explanation", "distinguishing_prediction"} and all(
                isinstance(v, str) and v.strip() for v in r.values()) for r in rivals):
        raise ValueError(f"{family}: rivals needs at least two {{explanation, distinguishing_prediction}} entries")
    updates = set()
    for variant, data in spec["variants"].items():
        where = f"{family}/{variant}"
        check_lines(data.get("update"), f"{where}/update", 3, 8)
        updates.add(text_bytes(data["update"]))
        if not isinstance(data.get("expected"), str) or not data["expected"].strip():
            raise ValueError(f"{where}: expected must describe the warranted answer")
        if variant != "original" and not str(data.get("relative_to_original", "")).strip():
            raise ValueError(f"{where}: relative_to_original must state the expected direction of change")
        ids = [item.get("id") for item in data.get("warranted", [])]
        if tuple(ids) != ITEMS:
            raise ValueError(f"{where}: warranted items must be exactly {ITEMS} in that order")
        fp_ids = [item.get("id") for item in data.get("false_promotions", [])]
        if not fp_ids or len(set(fp_ids)) != len(fp_ids) or not all(isinstance(i, str) and i for i in fp_ids):
            raise ValueError(f"{where}: false_promotions needs unique ids")
        docs = documents(spec, variant)
        for item in data["warranted"] + data["false_promotions"]:
            if "citation" in item:
                citation = item["citation"]
                lines = docs.get(citation.get("document"), b"").decode().splitlines()
                if not citation.get("any_lines") or not all(isinstance(i, int) and 1 <= i <= len(lines)
                                                             for i in citation["any_lines"]):
                    raise ValueError(f"{where}/{item['id']}: citation lines out of range")
                continue
            if item.get("field") not in ("answer", "limitation", "next") or not item.get("all"):
                raise ValueError(f"{where}/{item.get('id')}: needs a field and non-empty all clauses")
            unless = item.get("unless", [])
            for clause in item["all"] + ([] if unless == "hedges" else unless):
                re.compile(clause)
        if [i["id"] for i in data["warranted"] if "citation" in i] != ["decisive_citation"]:
            raise ValueError(f"{where}: only decisive_citation is a citation item")
        if not all(fp.get("unless") for fp in data["false_promotions"]):
            raise ValueError(f"{where}: every false promotion needs unless clauses (use \"hedges\")")
        rub = rubric(spec, variant)
        references = data.get("reference_answers")
        if not isinstance(references, list) or len(references) < 2:
            raise ValueError(f"{where}: give at least two differently phrased reference answers")
        for index, answer in enumerate(references):
            check_answer(answer, f"{where}/reference_answers[{index}]")
            result = grade(answer, rub, docs)
            missed = [k for k, v in result["correct"].items() if not v]
            promoted = [k for k, v in result["unwarranted"].items() if v]
            if missed or promoted:
                raise ValueError(f"{where}/reference_answers[{index}]: missed {missed}, false promotions {promoted}")
        if variant != "missing":
            # Describing the superseded review state must not cost a correct answer its
            # disposition: abstention filters have to target the current disposition only.
            for index, answer in enumerate(references):
                narrated = dict(answer, answer=f"{EARLIER_STATE} {answer['answer']}")
                if not grade(narrated, rub, docs)["correct"]["current_disposition"]:
                    raise ValueError(f"{where}/reference_answers[{index}]: prefixing a description of the "
                                     "earlier review state loses current_disposition; narrow the abstention filter")
        check_answer(data.get("overclaim_answer"), f"{where}/overclaim_answer")
        if not any(grade(data["overclaim_answer"], rub, docs)["unwarranted"].values()):
            raise ValueError(f"{where}/overclaim_answer: triggers no false promotion")
        if variant == "original" or variant == "cosmetic":
            for field in ("abstention_answer", "hedged_answer"):
                check_answer(data.get(field), f"{where}/{field}")
                if grade(data[field], rub, docs)["correct"]["current_disposition"]:
                    raise ValueError(f"{where}/{field}: an unwarranted abstention still scores current_disposition")
    if len(updates) != 2:
        raise ValueError(f"{family}: the twin's update must differ from the original's")
    # Cross-variant direction checks: the twin must be scoreable in the right direction.
    original = spec["variants"]["original"]["reference_answers"]
    twin_rubric = rubric(spec, twin)
    twin_docs = documents(spec, twin)
    if twin == "missing":
        # Unnecessary abstention must be scoreable too: the missing twin's warranted
        # (hedged) answers are unwarranted abstentions on the sufficient original.
        for index, answer in enumerate(spec["variants"]["missing"]["reference_answers"]):
            if grade(answer, rubric(spec, "original"), documents(spec, "original"))["correct"]["current_disposition"]:
                raise ValueError(f"{family}: missing-twin reference answer {index} still scores current_disposition "
                                 "on the original, so unnecessary abstention would go unscored")
    for index, answer in enumerate(original):
        result = grade(answer, twin_rubric, twin_docs)
        if twin == "missing" and not any(result["unwarranted"].values()):
            raise ValueError(f"{family}: original reference answer {index} is not a false promotion on the missing twin")
        if twin == "cosmetic":
            missed = [k for k, v in result["correct"].items() if not v and k != "decisive_citation"]
            if missed or any(result["unwarranted"].values()):
                raise ValueError(f"{family}: original reference answer {index} fails the cosmetic twin ({missed})")
    return spec


def check_authoring(directory, names=None):
    directory = Path(directory)
    files = [directory / n for n in names] if names else sorted(directory.glob("release-*.json"))
    specs = []
    failures = 0
    for file in files:
        try:
            specs.append(check_spec(load(file), file.name))
            print(f"ok   {file.name}")
        except (ValueError, KeyError, TypeError, AttributeError, re.error, json.JSONDecodeError) as error:
            failures += 1
            print(f"FAIL {file.name}: {error!r}")
    sources = Counter(sha256(text_bytes(s["source"])) for s in specs)
    for spec in specs:
        if sources[sha256(text_bytes(spec["source"]))] > 1:
            failures += 1
            print(f"FAIL {spec['family']}: source bytes shared with another family")
    return specs, failures


# --- Bundle --------------------------------------------------------------------

def bundle_files(spec):
    """Map bundle paths to bytes for one family."""
    family = spec["family"]
    files = {}
    key = {k: v for k, v in spec.items() if k not in ("source", "objective")}
    key["variants"] = {}
    for variant, data in spec["variants"].items():
        base = f"sealed/{family}/{variant}"
        files[f"{base}/objective.txt"] = (spec["objective"].strip() + "\n").encode("utf-8")
        files[f"{base}/source.txt"] = text_bytes(spec["source"])
        files[f"{base}/update.txt"] = text_bytes(data["update"])
        entry = {k: v for k, v in data.items() if k != "update"}
        entry.update(rubric(spec, variant))
        key["variants"][variant] = entry
    files[f"evaluator/sealed/{family}.json"] = dump(key).encode("utf-8")
    files[f"authoring/{family}.json"] = dump(spec).encode("utf-8")
    return files


def deterministic_tar(files):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for path in sorted(files):
            info = tarfile.TarInfo(path)
            info.size = len(files[path])
            info.mtime = 0
            info.mode = 0o644
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            archive.addfile(info, io.BytesIO(files[path]))
    return gzip.compress(buffer.getvalue(), compresslevel=9, mtime=0)


def key_path(argument):
    path = Path(argument or os.environ.get("INVESTIGATION_SEALED_KEY") or DEFAULT_KEY).expanduser()
    if not path.is_file():
        raise SystemExit(f"Passphrase file not found: {path}")
    if path.resolve().is_relative_to(ROOT.resolve()):
        raise SystemExit("The passphrase file must live outside the repository")
    if path.stat().st_mode & 0o077:
        raise SystemExit(f"Passphrase file must not be readable by group or others: {path}")
    return path


def openssl(mode, key, data):
    result = subprocess.run([*OPENSSL, mode, "-pass", f"file:{key}"], input=data, capture_output=True)
    if result.returncode != 0:
        raise SystemExit(f"openssl {mode} failed: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout


def registration(spec, files):
    family = spec["family"]
    variants = []
    for variant in ("original", spec["twin"]):
        base = f"sealed/{family}/{variant}"
        variants.append({
            "id": variant,
            "split": "sealed",
            "directory": base,
            "objective_sha256": sha256(files[f"{base}/objective.txt"]),
            "initial": {"file": "source.txt", "sha256": sha256(files[f"{base}/source.txt"])},
            "update": {"file": "update.txt", "sha256": sha256(files[f"{base}/update.txt"])},
        })
    return {"id": family, "split": "sealed", "domain": spec["domain"],
            "key": f"sealed/{family}.json", "variants": variants}


def append_registrations(entries):
    """Append entries to the registry text without reformatting existing registrations."""
    text = REGISTRY.read_text(encoding="utf-8")
    end = text.rstrip().rfind("]")
    if end < 0 or not re.fullmatch(r"\]\s*\}\s*", text[end:]):
        raise ValueError("Unexpected registry layout")
    body = text[:end].rstrip()
    lines = []
    for entry in entries:
        variants = ",\n".join("        " + json.dumps(v, ensure_ascii=False) for v in entry["variants"])
        head = {k: entry[k] for k in ("id", "split", "domain", "key")}
        lines.append("    " + json.dumps(head, ensure_ascii=False)[:-1] + ', "variants": [\n' + variants + "\n    ]}")
    updated = body + ",\n" + ",\n".join(lines) + "\n" + text[end:]
    if json.loads(updated)["families"][-len(entries):] != entries:
        raise ValueError("Registry append did not round-trip")
    REGISTRY.write_text(updated, encoding="utf-8")


def non_sealed_source_digests(registry):
    digests = set()
    for family in registry["families"]:
        if family["split"] == "sealed":
            continue
        for variant in family["variants"]:
            directory = FIXTURES / variant["directory"]
            for file in directory.rglob("*") if directory.is_dir() else ():
                if file.is_file():
                    digests.add(sha256(file.read_bytes()))
    return digests


def seal(authoring, key, authors):
    specs, failures = check_authoring(authoring)
    if failures:
        raise SystemExit(f"{failures} authoring failures; nothing sealed")
    counts = check_counts([(s["family"], s["domain"], s["twin"]) for s in specs])
    registry = load(REGISTRY)
    if any(f["split"] == "sealed" for f in registry["families"]):
        raise SystemExit("Sealed families are already registered; a new sealed set needs new family IDs and a new manifest")
    files = {}
    for spec in specs:
        files.update(bundle_files(spec))
    taken = non_sealed_source_digests(registry)
    for path, data in files.items():
        if path.startswith("sealed/") and not path.endswith("objective.txt") and sha256(data) in taken:
            raise SystemExit(f"{path} reuses development or pilot bytes")
    plaintext = deterministic_tar(files)
    ciphertext = openssl("-e", key, plaintext)
    if openssl("-d", key, ciphertext) != plaintext:
        raise SystemExit("Round trip through openssl failed")
    entries = [registration(spec, files) for spec in specs]
    registry["families"].extend(entries)
    REGISTRY_CHECK["validate"](registry, FIXTURES)
    families = []
    for spec, entry in zip(specs, entries):
        family = spec["family"]
        families.append({
            "id": family,
            "domain": spec["domain"],
            "twin": spec["twin"],
            "key": entry["key"],
            "key_sha256": sha256(files[f"evaluator/sealed/{family}.json"]),
            "variants": {v["id"]: {"objective_sha256": v["objective_sha256"],
                                   "source_sha256": v["initial"]["sha256"],
                                   "update_sha256": v["update"]["sha256"]} for v in entry["variants"]},
        })
    manifest = {
        "study": "Sealed release trial case families (W14.01)",
        "preregistration": counts,
        "authorship": authors,
        "bundle": {
            "file": CIPHERTEXT.name,
            "ciphertext_sha256": sha256(ciphertext),
            "plaintext_sha256": sha256(plaintext),
            "plaintext_files": len(files),
            "format": "deterministic PAX tar (mtime 0, uid 0, sorted paths), gzip mtime 0",
            "encryption": " ".join(OPENSSL) + " -pass file:<passphrase>",
            "layout": {
                "sealed/<family>/<variant>/objective.txt": "participant objective (given in the prompt, never as a file)",
                "sealed/<family>/<variant>/source.txt": "initial seed source",
                "sealed/<family>/<variant>/update.txt": "evidence staged after the review checkpoint",
                "evaluator/sealed/<family>.json": "grading key: rubric per variant, rivals, handoff, reference answers",
                "authoring/<family>.json": "the author's specification the other files were generated from",
            },
            "passphrase": "outside the repository; default " + str(DEFAULT_KEY) + " or $INVESTIGATION_SEALED_KEY",
        },
        "families": families,
    }
    RELEASE.mkdir(parents=True, exist_ok=True)
    CIPHERTEXT.write_bytes(ciphertext)
    MANIFEST.write_text(dump(manifest), encoding="utf-8")
    append_registrations(entries)
    print(f"Sealed {len(specs)} families ({len(files)} files) into {CIPHERTEXT.relative_to(ROOT)}")


# --- Verification ----------------------------------------------------------------

def verify_public():
    registry = load(REGISTRY)
    manifest = load(MANIFEST)
    REGISTRY_CHECK["validate"](registry, FIXTURES)
    sealed = [f for f in registry["families"] if f["split"] == "sealed"]
    by_id = {f["id"]: f for f in manifest["families"]}
    if {f["id"] for f in sealed} != set(by_id) or len(by_id) != len(manifest["families"]):
        raise ValueError("Registry and manifest list different sealed families")
    rows = []
    for family in sealed:
        entry = by_id[family["id"]]
        match = FAMILY_ID.fullmatch(family["id"])
        if not match or family["domain"] != match.group(1) or entry["domain"] != family["domain"]:
            raise ValueError(f"Opaque family ID and domain disagree: {family['id']}")
        if [v["id"] for v in family["variants"]] != ["original", entry["twin"]] or entry["twin"] not in TWINS:
            raise ValueError(f"{family['id']}: needs exactly an original and its preregistered twin")
        if family["key"] != entry["key"] or not SHA.fullmatch(entry["key_sha256"]):
            raise ValueError(f"{family['id']}: key locator or digest")
        if len({v["initial"]["sha256"] for v in family["variants"]}) != 1:
            raise ValueError(f"{family['id']}: twins must share the initial source bytes")
        if len({v["update"]["sha256"] for v in family["variants"]}) != 2:
            raise ValueError(f"{family['id']}: twin updates must differ")
        for variant in family["variants"]:
            committed = entry["variants"][variant["id"]]
            if (committed["objective_sha256"], committed["source_sha256"], committed["update_sha256"]) != (
                    variant["objective_sha256"], variant["initial"]["sha256"], variant["update"]["sha256"]):
                raise ValueError(f"{family['id']}/{variant['id']}: registry and manifest digests differ")
        rows.append((family["id"], family["domain"], entry["twin"]))
    counts = check_counts(rows)
    if manifest["preregistration"]["families"] != counts["families"]:
        raise ValueError("Manifest counts differ from the preregistration")
    initial = Counter(f["variants"][0]["initial"]["sha256"] for f in sealed)
    if max(initial.values()) > 1:
        raise ValueError("Two sealed families share source bytes")
    if sha256(CIPHERTEXT.read_bytes()) != manifest["bundle"]["ciphertext_sha256"]:
        raise ValueError("Ciphertext digest does not match the manifest")
    stray = {p.name for p in RELEASE.iterdir()} - ALLOWED_RELEASE_FILES
    if stray:
        raise ValueError(f"Unexpected files beside the sealed bundle: {sorted(stray)}")
    if (FIXTURES / "sealed").exists() or (ROOT / "training/a-gate/evaluator/sealed").exists():
        raise ValueError("Sealed material is present in plain text in the checkout")
    return registry, manifest, Counter((d, t) for _, d, t in rows)


def unpack(key, destination):
    manifest = load(MANIFEST)
    plaintext = openssl("-d", key, CIPHERTEXT.read_bytes())
    if sha256(plaintext) != manifest["bundle"]["plaintext_sha256"]:
        raise ValueError("Decrypted bundle digest does not match the manifest")
    with tarfile.open(fileobj=io.BytesIO(gzip.decompress(plaintext)), mode="r:") as archive:
        archive.extractall(destination, filter="data")
    for path in Path(destination).rglob("*"):
        path.chmod(0o700 if path.is_dir() else 0o400)
    return manifest


def verify_sealed(key):
    registry, manifest, _ = verify_public()
    with tempfile.TemporaryDirectory(prefix="investigation-sealed-") as directory:
        root = Path(directory)
        root.chmod(0o700)
        unpack(key, root)
        names = sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file())
        if len(names) != manifest["bundle"]["plaintext_files"]:
            raise ValueError("Bundle file count differs from the manifest")
        REGISTRY_CHECK["validate"](registry, FIXTURES, root)
        for entry in manifest["families"]:
            family = entry["id"]
            key_bytes = (root / "evaluator" / entry["key"]).read_bytes()
            if sha256(key_bytes) != entry["key_sha256"]:
                raise ValueError(f"{family}: key differs from its committed digest")
            spec = load(root / "authoring" / f"{family}.json")
            check_spec(spec, f"{family}.json")
            if bundle_files(spec)[f"evaluator/sealed/{family}.json"] != key_bytes:
                raise ValueError(f"{family}: key was not generated from the authored specification")
            for variant, digests in entry["variants"].items():
                base = root / "sealed" / family / variant
                if sha256((base / "objective.txt").read_bytes()) != digests["objective_sha256"]:
                    raise ValueError(f"{family}/{variant}: objective digest")
            grader_key = load(root / "evaluator" / entry["key"])
            assert set(grader_key["variants"]) == set(entry["variants"])
        taken = non_sealed_source_digests(registry)
        for name in names:
            if name.startswith("sealed/") and not name.endswith("objective.txt") and sha256((root / name).read_bytes()) in taken:
                raise ValueError(f"{name} reuses development or pilot bytes")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check-authoring", help="validate authored family specifications")
    check.add_argument("directory", type=Path)
    check.add_argument("files", nargs="*")
    sealing = commands.add_parser("seal", help="bundle, encrypt and register an authored set")
    sealing.add_argument("authoring", type=Path)
    sealing.add_argument("--key-file")
    sealing.add_argument("--authors", type=Path, required=True, help="JSON authorship declaration")
    verifying = commands.add_parser("verify", help="check counts and commitments; with a key, every digest and rubric")
    verifying.add_argument("--key-file")
    verifying.add_argument("--public", action="store_true", help="skip the keyed checks even if a key exists")
    opening = commands.add_parser("open", help="decrypt the bundle into an evaluator-only directory")
    opening.add_argument("destination", type=Path)
    opening.add_argument("--key-file")
    args = parser.parse_args()
    if args.command == "check-authoring":
        specs, failures = check_authoring(args.directory, args.files or None)
        print(f"{len(specs)} valid, {failures} failing")
        sys.exit(1 if failures else 0)
    if args.command == "seal":
        seal(args.authoring, key_path(args.key_file), load(args.authors))
    elif args.command == "verify":
        available = args.key_file or os.environ.get("INVESTIGATION_SEALED_KEY") or DEFAULT_KEY.expanduser().is_file()
        if args.public or not available:
            _, manifest, twins = verify_public()
            print(f"Verified {len(manifest['families'])} sealed family commitments against "
                  f"{manifest['preregistration']['source']} without opening the seal: "
                  + ", ".join(f"{d} {t} {n}" for (d, t), n in sorted(twins.items())))
        else:
            manifest = verify_sealed(key_path(args.key_file))
            print(f"Opened the seal privately and verified {len(manifest['families'])} families, "
                  f"{manifest['bundle']['plaintext_files']} files, every committed digest and rubric.")
    else:
        destination = args.destination.resolve()
        if destination.is_relative_to(ROOT.resolve()):
            raise SystemExit("Open the seal outside the repository")
        destination.mkdir(mode=0o700)
        unpack(key_path(args.key_file), destination)
        print(f"Opened into {destination}")


if __name__ == "__main__":
    main()
