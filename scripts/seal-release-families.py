#!/usr/bin/env python3
"""Check, seal, verify and open the sealed release-trial case families (W14.01),
the skip-the-tool lookup tasks sealed beside them (W14.02) and the sealed
instances of trial tasks 5-10 (W14.05b), which have a bundle of their own.

The sealed families' source bytes, objectives and grading keys never appear in
the checkout as plain text. The checkout holds only:

* opaque registrations in fixtures/family-registry.json (split ``sealed``,
  per-variant digests of objective, source and update bytes, key locator);
* training/release/families/manifest.json, which commits the digest of every
  key, objective and source file, the plaintext bundle digest and the
  preregistered counts it was checked against;
* training/release/families/sealed.tar.gz.enc, the bundle encrypted with a
  passphrase kept outside the repository;
* once sealed, training/release/families/trial-tasks.tar.gz.enc, the trial-task
  instances encrypted with the same passphrase, and the manifest's
  ``trial_tasks`` section: opaque instance IDs, per-instance digests, the
  index digest and one digest over the whole opened tree. No path, family name
  or source byte of an instance is committed in plain text.

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
# Skip-the-tool probes (W14.02): one-shot lookups and ordinary implementation tasks,
# registered in the sealed split under domain "lookup" with a single "lookup" variant.
LOOKUP_ID = re.compile(r"^release-lookup-(\d{2})$")
LOOKUP_KINDS = ("fact", "implementation")
LOOKUP_ITEMS = ("current_disposition", "decisive_citation", "material_limitation", "discriminating_followup")
LOOKUP_VARIANT = "lookup"
DIRECT_LOOKUP = "This is a direct lookup from the source and has not been independently verified."
LOOKUP_PREREGISTRATION = "Separate lookup probes from this primary test."
SHA = re.compile(r"^[a-f0-9]{64}$")
OPENSSL = ["openssl", "enc", "-aes-256-cbc", "-pbkdf2", "-iter", "600000", "-md", "sha256"]
TRIAL_TASK_CIPHERTEXT = RELEASE / "trial-tasks.tar.gz.enc"
ALLOWED_RELEASE_FILES = {"README.md", "manifest.json", "sealed.tar.gz.enc", TRIAL_TASK_CIPHERTEXT.name}
# Sealed trial-task instances (W14.05b): opaque IDs, one per task-5 to task-10 instance.
TRIAL_TASK_ID = re.compile(r"^release-t(5|6|7|8|9|10)-(\d{2})$")
TRIAL_TASK_ROOT = "trial-tasks"
TRIAL_TASK_INDEX = "trial-tasks.json"
TRIAL_TASK_NUMBERS = ("5", "6", "7", "8", "9", "10")
TRIAL_TASK_PATH = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*(?:/[A-Za-z0-9][A-Za-z0-9._-]*)*$")

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


# --- Skip-the-tool lookup tasks (W14.02) -------------------------------------------

def lookup_rubric(spec):
    return {"warranted": expand(spec["warranted"]), "false_promotions": expand(spec["false_promotions"])}


def lookup_documents(spec):
    return {"source.txt": text_bytes(spec["source"])}


def check_lookup_spec(spec, name=None):
    """Validate one authored skip-the-tool task: a direct answer is warranted from source.txt alone."""
    task = spec.get("family", "")
    if not LOOKUP_ID.fullmatch(task) or (name and name != f"{task}.json"):
        raise ValueError(f"{name}: family must be release-lookup-NN and match the file name")
    if spec.get("domain") != "lookup":
        raise ValueError(f"{task}: domain must be lookup")
    if spec.get("kind") not in LOOKUP_KINDS:
        raise ValueError(f"{task}: kind must be one of {LOOKUP_KINDS}")
    if spec.get("theme") not in DOMAINS:
        raise ValueError(f"{task}: theme must be one of {DOMAINS}")
    for field in ("title", "objective", "expected", "why_direct"):
        if not isinstance(spec.get(field), str) or not spec[field].strip() or "\n" in spec[field]:
            raise ValueError(f"{task}: {field} must be a one-line string")
    if len(spec["objective"]) > 400:
        raise ValueError(f"{task}: objective is too long")
    check_lines(spec["source"], f"{task}/source", 4, 14)
    ids = [item.get("id") for item in spec.get("warranted", [])]
    if tuple(ids) != LOOKUP_ITEMS:
        raise ValueError(f"{task}: warranted items must be exactly {LOOKUP_ITEMS} in that order")
    fp_ids = [item.get("id") for item in spec.get("false_promotions", [])]
    if len(fp_ids) < 2 or len(set(fp_ids)) != len(fp_ids) or not all(isinstance(i, str) and i for i in fp_ids):
        raise ValueError(f"{task}: false_promotions needs at least two unique ids")
    docs = lookup_documents(spec)
    lines = docs["source.txt"].decode().splitlines()
    for item in spec["warranted"] + spec["false_promotions"]:
        if "citation" in item:
            citation = item["citation"]
            if citation.get("document") != "source.txt" or not citation.get("any_lines") or not all(
                    isinstance(i, int) and 1 <= i <= len(lines) for i in citation["any_lines"]):
                raise ValueError(f"{task}/{item['id']}: a citation names source.txt lines in range")
            continue
        if item.get("field") not in ("answer", "limitation", "next") or not item.get("all"):
            raise ValueError(f"{task}/{item.get('id')}: needs a field and non-empty all clauses")
        unless = item.get("unless", [])
        for clause in item["all"] + ([] if unless == "hedges" else unless):
            re.compile(clause)
    if [i["id"] for i in spec["warranted"] if "citation" in i] != ["decisive_citation"]:
        raise ValueError(f"{task}: only decisive_citation is a citation item")
    if not all(fp.get("unless") for fp in spec["false_promotions"]):
        raise ValueError(f"{task}: every false promotion needs unless clauses (use \"hedges\")")
    rub = lookup_rubric(spec)
    references = spec.get("reference_answers")
    if not isinstance(references, list) or len(references) < 2:
        raise ValueError(f"{task}: give at least two differently phrased reference answers")
    for index, answer in enumerate(references):
        check_answer(answer, f"{task}/reference_answers[{index}]")
        result = grade(answer, rub, docs)
        missed = [k for k, v in result["correct"].items() if not v]
        promoted = [k for k, v in result["unwarranted"].items() if v]
        if missed or promoted:
            raise ValueError(f"{task}/reference_answers[{index}]: missed {missed}, false promotions {promoted}")
        # Labelling the answer as an unverified direct lookup is honest, not an abstention.
        labelled = dict(answer, answer=f"{DIRECT_LOOKUP} {answer['answer']}")
        if not grade(labelled, rub, docs)["correct"]["current_disposition"]:
            raise ValueError(f"{task}/reference_answers[{index}]: labelling it a direct, unverified lookup "
                             "loses current_disposition; narrow the abstention filter")
    check_answer(spec.get("overclaim_answer"), f"{task}/overclaim_answer")
    if not any(grade(spec["overclaim_answer"], rub, docs)["unwarranted"].values()):
        raise ValueError(f"{task}/overclaim_answer: triggers no false promotion")
    check_answer(spec.get("abstention_answer"), f"{task}/abstention_answer")
    if grade(spec["abstention_answer"], rub, docs)["correct"]["current_disposition"]:
        raise ValueError(f"{task}/abstention_answer: an unnecessary abstention still scores current_disposition")
    return spec


def check_lookups(directory, names=None):
    directory = Path(directory)
    files = [directory / n for n in names] if names else sorted(directory.glob("release-lookup-*.json"))
    specs = []
    failures = 0
    for file in files:
        try:
            specs.append(check_lookup_spec(load(file), file.name))
            print(f"ok   {file.name}")
        except (ValueError, KeyError, TypeError, AttributeError, re.error, json.JSONDecodeError) as error:
            failures += 1
            print(f"FAIL {file.name}: {error!r}")
    sources = Counter(sha256(text_bytes(s["source"])) for s in specs)
    for spec in specs:
        if sources[sha256(text_bytes(spec["source"]))] > 1:
            failures += 1
            print(f"FAIL {spec['family']}: source bytes shared with another task")
    return specs, failures


def lookup_bundle_files(spec):
    task = spec["family"]
    base = f"sealed/{task}/{LOOKUP_VARIANT}"
    variant = {k: spec[k] for k in ("expected", "reference_answers", "overclaim_answer", "abstention_answer")}
    variant.update(lookup_rubric(spec))
    key = {k: spec[k] for k in ("family", "domain", "kind", "theme", "title", "why_direct")}
    key["variants"] = {LOOKUP_VARIANT: variant}
    return {
        f"{base}/objective.txt": (spec["objective"].strip() + "\n").encode("utf-8"),
        f"{base}/source.txt": text_bytes(spec["source"]),
        f"evaluator/sealed/{task}.json": dump(key).encode("utf-8"),
        f"authoring/{task}.json": dump(spec).encode("utf-8"),
    }


def lookup_registration(spec, files):
    task = spec["family"]
    base = f"sealed/{task}/{LOOKUP_VARIANT}"
    return {"id": task, "split": "sealed", "domain": "lookup", "key": f"sealed/{task}.json",
            "variants": [{
                "id": LOOKUP_VARIANT, "split": "sealed", "directory": base,
                "objective_sha256": sha256(files[f"{base}/objective.txt"]),
                "initial": {"file": "source.txt", "sha256": sha256(files[f"{base}/source.txt"])},
            }]}


def lookup_preregistration():
    """The preregistration separates lookup probes from the primary test; it sets no count for them."""
    try:
        text = subprocess.run(["git", "-C", str(ROOT), "show", f"{PREREGISTRATION_COMMIT}:{PREREGISTRATION}"],
                              check=True, capture_output=True, text=True).stdout
    except (OSError, subprocess.CalledProcessError):
        text = (ROOT / PREREGISTRATION).read_text(encoding="utf-8")
    if LOOKUP_PREREGISTRATION not in text:
        raise ValueError("The preregistration no longer separates lookup probes from the primary test")
    return LOOKUP_PREREGISTRATION


def seal_lookups(authoring, key, authors):
    """Add the skip-the-tool tasks to the sealed bundle without changing any family's bytes."""
    specs, failures = check_lookups(authoring)
    if failures or not specs:
        raise SystemExit(f"{failures} authoring failures ({len(specs)} valid); nothing sealed")
    registry = load(REGISTRY)
    manifest = load(MANIFEST)
    if any(f["split"] == "sealed" and f["domain"] == "lookup" for f in registry["families"]) or "lookup_tasks" in manifest:
        raise SystemExit("Sealed lookup tasks are already registered; new tasks need new IDs and a new manifest")
    verify_public()
    previous = dict(manifest["bundle"])
    old_plaintext = openssl("-d", key, CIPHERTEXT.read_bytes())
    if sha256(old_plaintext) != previous["plaintext_sha256"]:
        raise SystemExit("Decrypted bundle digest does not match the manifest")
    files = {}
    with tarfile.open(fileobj=io.BytesIO(gzip.decompress(old_plaintext)), mode="r:") as archive:
        for member in archive.getmembers():
            if not member.isfile():
                raise SystemExit(f"Unexpected bundle member: {member.name}")
            files[member.name] = archive.extractfile(member).read()
    if len(files) != previous["plaintext_files"]:
        raise SystemExit("Bundle file count differs from the manifest")
    taken = non_sealed_source_digests(registry) | {sha256(data) for path, data in files.items()
                                                   if path.startswith("sealed/") and not path.endswith("objective.txt")}
    added = {}
    for spec in specs:
        for path, data in lookup_bundle_files(spec).items():
            if path in files or path in added:
                raise SystemExit(f"{path} is already in the bundle")
            if path.startswith("sealed/") and not path.endswith("objective.txt") and sha256(data) in taken:
                raise SystemExit(f"{path} reuses registered source bytes")
            added[path] = data
    files.update(added)
    plaintext = deterministic_tar(files)
    ciphertext = openssl("-e", key, plaintext)
    if openssl("-d", key, ciphertext) != plaintext:
        raise SystemExit("Round trip through openssl failed")
    entries = [lookup_registration(spec, added) for spec in specs]
    registry["families"].extend(entries)
    REGISTRY_CHECK["validate"](registry, FIXTURES)
    manifest["bundle"].update(
        ciphertext_sha256=sha256(ciphertext), plaintext_sha256=sha256(plaintext), plaintext_files=len(files),
        previous={k: previous[k] for k in ("ciphertext_sha256", "plaintext_sha256", "plaintext_files")}
        | {"tar_sha256": sha256(gzip.decompress(old_plaintext)),
           "note": "W14.01 bundle before the lookup tasks were added. Keyed verify rebuilds the uncompressed "
                   "tar from the bundle's non-lookup files and checks it against tar_sha256, so no family "
                   "file changed."})
    manifest["bundle"]["layout"].update({
        "sealed/<lookup>/lookup/objective.txt": "skip-the-tool task objective (given in the prompt, never as a file)",
        "sealed/<lookup>/lookup/source.txt": "the task's only source; a lookup task has no update.txt",
    })
    manifest["lookup_tasks"] = {
        "study": "Skip-the-tool tasks for the sealed release trial (W14.02)",
        "preregistration": {"source": f"{PREREGISTRATION}@{PREREGISTRATION_COMMIT}", "quote": lookup_preregistration(),
                            "count": "not preregistered; fixed by this manifest before any run"},
        "counts": {"tasks": len(specs),
                   "by_kind": dict(sorted(Counter(s["kind"] for s in specs).items())),
                   "by_theme": dict(sorted(Counter(s["theme"] for s in specs).items()))},
        "report": {
            "separation": "Diagnostic, reported apart from the primary estimate and the release rule.",
            "runs": "Both arms and both pinned models per task, same budgets and fresh sessions as the families.",
            "outcomes": "Per arm: warranted items met, missed warranted items and false promotions, with denominators.",
            "cost": "Per arm and paired CLI minus notes: metered tokens, shell calls, wall time, "
                    "help/schema/onboarding bytes and CLI calls.",
            "unnecessary_tool_use": "CLI arm: any committed journal event in the participant workspace; report the rate.",
        },
        "authorship": authors,
        "tasks": [{"id": spec["family"], "kind": spec["kind"], "theme": spec["theme"], "key": entry["key"],
                   "key_sha256": sha256(added[f"evaluator/sealed/{spec['family']}.json"]),
                   "variants": {LOOKUP_VARIANT: {"objective_sha256": entry["variants"][0]["objective_sha256"],
                                                 "source_sha256": entry["variants"][0]["initial"]["sha256"]}}}
                  for spec, entry in zip(specs, entries)],
    }
    CIPHERTEXT.write_bytes(ciphertext)
    MANIFEST.write_text(dump(manifest), encoding="utf-8")
    append_registrations(entries)
    print(f"Sealed {len(specs)} lookup tasks; bundle now holds {len(files)} files")


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
    return gzip.compress(deterministic_raw_tar(files), compresslevel=9, mtime=0)


def deterministic_raw_tar(files):
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
    return buffer.getvalue()


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
    try:
        result = subprocess.run([*OPENSSL, mode, "-pass", f"file:{key}"], input=data, capture_output=True)
    except FileNotFoundError:
        raise SystemExit("openssl is not on PATH: sealed bundles are `openssl enc -aes-256-cbc -pbkdf2` "
                         "ciphertext, so sealing and opening need it") from None
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


# --- Sealed trial-task instances (W14.05b) ------------------------------------------------------
#
# An authoring directory holds `trial-tasks.json`, the index the trial harness takes as
# `--trial-tasks`: {"5": [paths], ..., "10": [paths], "source_root": DIR, "key_root": DIR}, every path
# relative to the index. Tasks 5-7 are trial-task.v1 files (training/trial-tasks/protocol.md) whose
# `family` is an opaque ID, with sources at <source_root>/<family>/<variant>/ and the rubric key at
# <key_root>/<key>. Tasks 8-10 are instance files (training/trial-tasks/tasks-8-10/protocol.md) whose
# `id` is an opaque ID and whose sources are `from_instance_dir`. The whole directory is sealed as one
# deterministic tar under `trial-tasks/` and encrypted with the families' passphrase.

def tree_digest(files):
    """One digest over (path, content digest) pairs: what the manifest commits for the opened tree."""
    lines = "".join(f"{sha256(files[path])}  {path}\n" for path in sorted(files))
    return sha256(lines.encode("utf-8"))


def read_tree(root):
    root = Path(root)
    files = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            raise ValueError(f"{relative}: symbolic links cannot be sealed")
        if path.is_file():
            if not TRIAL_TASK_PATH.fullmatch(relative):
                raise ValueError(f"{relative}: use plain path segments (letters, digits, '.', '_', '-')")
            files[relative] = path.read_bytes()
    return files


def trial_task_index(root):
    """The index with its paths resolved inside `root`; refuses paths that leave it."""
    root = Path(root).resolve()
    index = load(root / TRIAL_TASK_INDEX)
    unknown = set(index) - set(TRIAL_TASK_NUMBERS) - {"source_root", "key_root"}
    if unknown:
        raise ValueError(f"{TRIAL_TASK_INDEX}: unknown keys {sorted(unknown)}")

    def inside(relative, what):
        path = (root / relative).resolve()
        if not path.is_relative_to(root):
            raise ValueError(f"{what} leaves the authoring directory: {relative}")
        return path
    roots = {name: inside(index.get(name, "."), name) for name in ("source_root", "key_root")}
    tasks = {number: [inside(item, f"task {number}") for item in index.get(number, [])] for number in TRIAL_TASK_NUMBERS}
    return index, roots, tasks


_TRIAL_TASKS = {}


def trial_task_module():
    if not _TRIAL_TASKS:
        _TRIAL_TASKS.update(runpy.run_path(str(ROOT / "scripts/trial-tasks.py")))
    return _TRIAL_TASKS


def trial_task_inputs(number, path, roots):
    """(opaque ID, every file the instance reads) for one instance; refuses repository sources."""
    task = load(path)
    if str(task.get("task")) != number:
        raise ValueError(f"{path.name}: task {task.get('task')}, listed as task {number}")
    if task.get("split") != "sealed":
        raise ValueError(f"{path.name}: split must be sealed")
    files = [path]
    identifier = task.get("family" if number in ("5", "6", "7") else "id", "")
    match = TRIAL_TASK_ID.fullmatch(identifier)
    if not match or match.group(1) != number:
        raise ValueError(f"{path.name}: the instance ID must be opaque, release-t{number}-NN (got {identifier!r})")
    if number in ("5", "6", "7"):
        tt = trial_task_module()
        loaded = tt["load_task"](str(path), roots["source_root"], roots["key_root"])
        key = tt["key_path"](loaded)
        if not key.is_file():
            raise ValueError(f"{identifier}: key {task.get('key')} not found under key_root")
        sources = tt["source_files"](loaded)
        if not sources:
            raise ValueError(f"{identifier}: no sources under source_root/{identifier}/")
        files += [key, *sources]
    else:
        setup = task.get("setup", {})
        if "drill" in setup:
            raise ValueError(f"{identifier}: a sealed instance carries its own steps, not a repository drill")
        for source in setup.get("base", {}).get("sources", []):
            if "from_instance_dir" not in source:
                raise ValueError(f"{identifier}: sealed sources are `from_instance_dir`, not repository files")
            files.append(path.parent / source["from_instance_dir"])
    return identifier, files


def development_trial_task_digests():
    digests = set()
    for directory in (FIXTURES / "development/trial-tasks", ROOT / "training/trial-tasks"):
        for path in directory.rglob("*"):
            if path.is_file():
                digests.add(sha256(path.read_bytes()))
    return digests


def check_trial_task_set(authoring):
    """Structure, opaque IDs and fresh bytes for an authoring directory. Returns (files, tasks)."""
    root = Path(authoring).resolve()
    files = read_tree(root)
    if TRIAL_TASK_INDEX not in files:
        raise ValueError(f"no {TRIAL_TASK_INDEX} in {root}")
    _, roots, tasks = trial_task_index(root)
    listed, used = [], {root / TRIAL_TASK_INDEX}
    for number in TRIAL_TASK_NUMBERS:
        for path in tasks[number]:
            identifier, inputs = trial_task_inputs(number, path, roots)
            for file in inputs:
                if not Path(file).resolve().is_relative_to(root) or not Path(file).is_file():
                    raise ValueError(f"{identifier}: input outside the authoring directory or missing")
                used.add(Path(file).resolve())
            listed.append({"task": int(number), "id": identifier,
                           "sha256": sha256(path.read_bytes()),
                           "file": path.relative_to(root).as_posix()})
    if not listed:
        raise ValueError("the index lists no instance")
    if len({entry["id"] for entry in listed}) != len(listed):
        raise ValueError("two instances share an ID")
    stray = sorted(set(files) - {path.relative_to(root).as_posix() for path in used})
    if stray:
        raise ValueError(f"{len(stray)} files are not read by any listed instance; remove them before sealing")
    registry = load(REGISTRY)
    taken = non_sealed_source_digests(registry) | development_trial_task_digests()
    for family in registry["families"]:
        for variant in family["variants"]:
            taken |= {variant[part]["sha256"] for part in ("initial", "update") if part in variant}
    reused = [path for path, data in files.items() if path != TRIAL_TASK_INDEX and sha256(data) in taken]
    if reused:
        raise ValueError(f"{len(reused)} files reuse registered, development or family bytes")
    return files, listed


def check_trial_tasks(authoring, binary):
    """check_trial_task_set plus the rehearsals on `binary`: tasks 5-7 through `trial-tasks.py check`
    and tasks 8-10 through `trial-tasks-8-10.py self-check` (correct path passes, every fault caught).
    Prints counts only; the sub-checks print their own failures."""
    _, listed = check_trial_task_set(authoring)
    root = Path(authoring).resolve()
    _, roots, tasks = trial_task_index(root)
    failures = 0
    early = [str(path) for number in ("5", "6", "7") for path in tasks[number]]
    if early:
        done = subprocess.run([sys.executable, str(ROOT / "scripts/trial-tasks.py"), "check", *early, "--bin", str(binary),
                               "--source-root", str(roots["source_root"]), "--key-root", str(roots["key_root"])],
                              capture_output=True, text=True, check=False)
        failures += done.returncode != 0
        if done.returncode != 0:
            print(done.stdout[-4000:] + done.stderr[-2000:])
    late = [str(path) for number in ("8", "9", "10") for path in tasks[number]]
    if late:
        done = subprocess.run([sys.executable, str(ROOT / "scripts/trial-tasks-8-10.py"), "self-check", "--bin",
                               str(binary), *late], capture_output=True, text=True, check=False)
        failures += done.returncode != 0
        if done.returncode != 0:
            print(done.stdout[-4000:] + done.stderr[-2000:])
    return listed, failures


def bundle_trial_tasks(authoring, key, authors, manifest_path=None, ciphertext_path=None):
    """Seal a checked authoring directory: a deterministic tar under trial-tasks/, encrypted with the
    families' passphrase, and the manifest's `trial_tasks` commitments. Decrypts nothing."""
    manifest_path = Path(manifest_path or MANIFEST)
    ciphertext_path = Path(ciphertext_path or TRIAL_TASK_CIPHERTEXT)
    manifest = load(manifest_path)
    if "trial_tasks" in manifest or ciphertext_path.exists():
        raise SystemExit("Sealed trial-task instances are already committed; new instances need new IDs and a new manifest")
    files, listed = check_trial_task_set(authoring)
    plaintext = deterministic_tar({f"{TRIAL_TASK_ROOT}/{path}": data for path, data in files.items()})
    ciphertext = openssl("-e", key, plaintext)
    if openssl("-d", key, ciphertext) != plaintext:
        raise SystemExit("Round trip through openssl failed")
    manifest["trial_tasks"] = {
        "study": "Sealed instances of release-trial tasks 5-10 (W14.05b)",
        "authorship": authors,
        "bundle": {
            "file": ciphertext_path.name,
            "ciphertext_sha256": sha256(ciphertext),
            "plaintext_sha256": sha256(plaintext),
            "plaintext_files": len(files),
            "format": "deterministic PAX tar (mtime 0, uid 0, sorted paths), gzip mtime 0",
            "encryption": " ".join(OPENSSL) + " -pass file:<passphrase> (the families' passphrase)",
            "opens_to": f"{TRIAL_TASK_ROOT}/ beside sealed/ and evaluator/ in the opened directory",
        },
        "index": {"path": f"{TRIAL_TASK_ROOT}/{TRIAL_TASK_INDEX}", "sha256": sha256(files[TRIAL_TASK_INDEX])},
        "tree": {"root": TRIAL_TASK_ROOT, "files": len(files), "sha256": tree_digest(files)},
        "counts": {number: sum(entry["task"] == int(number) for entry in listed) for number in TRIAL_TASK_NUMBERS},
        "tasks": [{k: entry[k] for k in ("task", "id", "sha256")} for entry in listed],
    }
    ciphertext_path.write_bytes(ciphertext)
    manifest_path.write_text(dump(manifest), encoding="utf-8")
    return manifest["trial_tasks"]


def verify_trial_tasks_public(manifest):
    section = manifest.get("trial_tasks")
    if section is None:
        if TRIAL_TASK_CIPHERTEXT.exists():
            raise ValueError("A trial-task bundle exists but the manifest commits none")
        return None
    if sha256(TRIAL_TASK_CIPHERTEXT.read_bytes()) != section["bundle"]["ciphertext_sha256"]:
        raise ValueError("Trial-task ciphertext digest does not match the manifest")
    ids = [entry["id"] for entry in section["tasks"]]
    if len(set(ids)) != len(ids) or any(not TRIAL_TASK_ID.fullmatch(i) or TRIAL_TASK_ID.fullmatch(i).group(1) !=
                                        str(entry["task"]) for i, entry in zip(ids, section["tasks"])):
        raise ValueError("Trial-task IDs must be unique and opaque")
    if section["counts"] != {n: sum(str(e["task"]) == n for e in section["tasks"]) for n in TRIAL_TASK_NUMBERS}:
        raise ValueError("Trial-task counts differ from the listed instances")
    return section


def unpack_trial_tasks(key, destination, section, ciphertext_path=None):
    plaintext = openssl("-d", key, Path(ciphertext_path or TRIAL_TASK_CIPHERTEXT).read_bytes())
    if sha256(plaintext) != section["bundle"]["plaintext_sha256"]:
        raise ValueError("Decrypted trial-task bundle digest does not match the manifest")
    with tarfile.open(fileobj=io.BytesIO(gzip.decompress(plaintext)), mode="r:") as archive:
        if any(not member.name.startswith(TRIAL_TASK_ROOT + "/") or not member.isfile() for member in archive):
            raise ValueError("Unexpected trial-task bundle member")
        archive.extractall(destination, filter="data")
    opened = Path(destination) / TRIAL_TASK_ROOT
    files = read_tree(opened)
    if len(files) != section["tree"]["files"] or tree_digest(files) != section["tree"]["sha256"]:
        raise ValueError("Opened trial-task tree differs from its committed digest")
    if sha256(files[TRIAL_TASK_INDEX]) != section["index"]["sha256"]:
        raise ValueError("Trial-task index differs from its committed digest")
    for path in opened.rglob("*"):
        path.chmod(0o700 if path.is_dir() else 0o400)
    opened.chmod(0o700)


# --- Verification ----------------------------------------------------------------

def verify_public():
    registry = load(REGISTRY)
    manifest = load(MANIFEST)
    REGISTRY_CHECK["validate"](registry, FIXTURES)
    sealed_all = [f for f in registry["families"] if f["split"] == "sealed"]
    sealed = [f for f in sealed_all if f["domain"] != "lookup"]
    verify_lookups_public([f for f in sealed_all if f["domain"] == "lookup"], manifest)
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
    initial = Counter(f["variants"][0]["initial"]["sha256"] for f in sealed_all)
    if max(initial.values()) > 1:
        raise ValueError("Two sealed families or tasks share source bytes")
    if sha256(CIPHERTEXT.read_bytes()) != manifest["bundle"]["ciphertext_sha256"]:
        raise ValueError("Ciphertext digest does not match the manifest")
    stray = {p.name for p in RELEASE.iterdir()} - ALLOWED_RELEASE_FILES
    if stray:
        raise ValueError(f"Unexpected files beside the sealed bundle: {sorted(stray)}")
    if (FIXTURES / "sealed").exists() or (ROOT / "training/a-gate/evaluator/sealed").exists():
        raise ValueError("Sealed material is present in plain text in the checkout")
    verify_trial_tasks_public(manifest)
    return registry, manifest, Counter((d, t) for _, d, t in rows)


def verify_lookups_public(sealed, manifest):
    """Registry and manifest agree on the skip-the-tool tasks; returns their (kind, theme) counts."""
    section = manifest.get("lookup_tasks")
    if section is None:
        if sealed:
            raise ValueError("Sealed lookup tasks are registered but the manifest lists none")
        return Counter()
    if section["preregistration"]["quote"] != lookup_preregistration():
        raise ValueError("Lookup tasks cite a different preregistration sentence")
    by_id = {t["id"]: t for t in section["tasks"]}
    if {f["id"] for f in sealed} != set(by_id) or len(by_id) != len(section["tasks"]):
        raise ValueError("Registry and manifest list different sealed lookup tasks")
    if section["counts"]["tasks"] != len(sealed):
        raise ValueError("Lookup task count differs from the manifest")
    for family in sealed:
        entry = by_id[family["id"]]
        if not LOOKUP_ID.fullmatch(family["id"]) or entry["kind"] not in LOOKUP_KINDS or entry["theme"] not in DOMAINS:
            raise ValueError(f"{family['id']}: lookup ID, kind or theme")
        if [v["id"] for v in family["variants"]] != [LOOKUP_VARIANT] or "update" in family["variants"][0]:
            raise ValueError(f"{family['id']}: a lookup task has one variant and no update")
        if family["key"] != entry["key"] or not SHA.fullmatch(entry["key_sha256"]):
            raise ValueError(f"{family['id']}: key locator or digest")
        variant, committed = family["variants"][0], entry["variants"][LOOKUP_VARIANT]
        if (committed["objective_sha256"], committed["source_sha256"]) != (
                variant["objective_sha256"], variant["initial"]["sha256"]):
            raise ValueError(f"{family['id']}: registry and manifest digests differ")
    counts = Counter((t["kind"], t["theme"]) for t in section["tasks"])
    if dict(sorted(Counter(t["kind"] for t in section["tasks"]).items())) != section["counts"]["by_kind"] or \
            dict(sorted(Counter(t["theme"] for t in section["tasks"]).items())) != section["counts"]["by_theme"]:
        raise ValueError("Lookup task counts by kind or theme differ from the manifest")
    return counts


def unpack(key, destination):
    manifest = load(MANIFEST)
    plaintext = openssl("-d", key, CIPHERTEXT.read_bytes())
    if sha256(plaintext) != manifest["bundle"]["plaintext_sha256"]:
        raise ValueError("Decrypted bundle digest does not match the manifest")
    with tarfile.open(fileobj=io.BytesIO(gzip.decompress(plaintext)), mode="r:") as archive:
        archive.extractall(destination, filter="data")
    for path in Path(destination).rglob("*"):
        path.chmod(0o700 if path.is_dir() else 0o400)
    if manifest.get("trial_tasks"):
        unpack_trial_tasks(key, destination, manifest["trial_tasks"])
    return manifest


def verify_sealed(key):
    registry, manifest, _ = verify_public()
    with tempfile.TemporaryDirectory(prefix="investigation-sealed-") as directory:
        root = Path(directory)
        root.chmod(0o700)
        unpack(key, root)
        names = sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()
                       and not p.relative_to(root).as_posix().startswith(TRIAL_TASK_ROOT + "/"))
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
        for entry in manifest.get("lookup_tasks", {}).get("tasks", []):
            task = entry["id"]
            key_bytes = (root / "evaluator" / entry["key"]).read_bytes()
            if sha256(key_bytes) != entry["key_sha256"]:
                raise ValueError(f"{task}: key differs from its committed digest")
            spec = check_lookup_spec(load(root / "authoring" / f"{task}.json"), f"{task}.json")
            if (spec["kind"], spec["theme"]) != (entry["kind"], entry["theme"]):
                raise ValueError(f"{task}: kind or theme differs from the manifest")
            if lookup_bundle_files(spec)[f"evaluator/sealed/{task}.json"] != key_bytes:
                raise ValueError(f"{task}: key was not generated from the authored specification")
            base = root / "sealed" / task / LOOKUP_VARIANT
            if sha256((base / "objective.txt").read_bytes()) != entry["variants"][LOOKUP_VARIANT]["objective_sha256"]:
                raise ValueError(f"{task}: objective digest")
            if (base / "update.txt").exists():
                raise ValueError(f"{task}: a lookup task has no update")
        previous = manifest["bundle"].get("previous")
        if previous:
            lookups = {path for t in manifest["lookup_tasks"]["tasks"] for path in (
                f"sealed/{t['id']}/{LOOKUP_VARIANT}/objective.txt", f"sealed/{t['id']}/{LOOKUP_VARIANT}/source.txt",
                f"evaluator/{t['key']}", f"authoring/{t['id']}.json")}
            earlier = {name: (root / name).read_bytes() for name in names if name not in lookups}
            if len(earlier) != previous["plaintext_files"] or sha256(deterministic_raw_tar(earlier)) != previous["tar_sha256"]:
                raise ValueError("The bundle's family files differ from the W14.01 bundle they were carried over from")
        taken = non_sealed_source_digests(registry)
        for name in names:
            if name.startswith("sealed/") and not name.endswith("objective.txt") and sha256((root / name).read_bytes()) in taken:
                raise ValueError(f"{name} reuses development or pilot bytes")
    return manifest


def lookup_summary(manifest):
    section = manifest.get("lookup_tasks")
    text = ""
    if section:
        kinds = ", ".join(f"{k} {n}" for k, n in section["counts"]["by_kind"].items())
        text = f"; {section['counts']['tasks']} skip-the-tool tasks ({kinds})"
    tasks = manifest.get("trial_tasks")
    if tasks:
        counts = ", ".join(f"task {n} {c}" for n, c in tasks["counts"].items() if c)
        text += f"; {len(tasks['tasks'])} trial-task instances ({counts})"
    return text


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
    lookups = commands.add_parser("check-lookups", help="validate authored skip-the-tool task specifications")
    lookups.add_argument("directory", type=Path)
    lookups.add_argument("files", nargs="*")
    sealing_lookups = commands.add_parser("seal-lookups", help="add authored skip-the-tool tasks to the sealed bundle")
    sealing_lookups.add_argument("authoring", type=Path)
    sealing_lookups.add_argument("--key-file")
    sealing_lookups.add_argument("--authors", type=Path, required=True, help="JSON authorship declaration")
    trial_check = commands.add_parser("check-trial-tasks", help="validate and rehearse authored task 5-10 instances")
    trial_check.add_argument("directory", type=Path)
    trial_check.add_argument("--bin", required=True, help="the binary the rehearsals run on")
    trial_seal = commands.add_parser("seal-trial-tasks", help="seal checked task 5-10 instances into their own bundle")
    trial_seal.add_argument("directory", type=Path)
    trial_seal.add_argument("--bin", required=True, help="the binary the rehearsals run on")
    trial_seal.add_argument("--key-file")
    trial_seal.add_argument("--authors", type=Path, required=True, help="JSON authorship declaration")
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
    if args.command == "check-lookups":
        specs, failures = check_lookups(args.directory, args.files or None)
        print(f"{len(specs)} valid, {failures} failing")
        sys.exit(1 if failures else 0)
    if args.command in ("check-trial-tasks", "seal-trial-tasks"):
        directory = args.directory.resolve()
        if directory.is_relative_to(ROOT.resolve()):
            raise SystemExit("Keep authored trial-task instances outside the repository")
        try:
            listed, failures = check_trial_tasks(directory, args.bin)
        except ValueError as error:
            raise SystemExit(f"check-trial-tasks: {error}") from None
        print(f"{len(listed)} instances checked, {failures} rehearsal failures")
        if failures:
            sys.exit(1)
        if args.command == "seal-trial-tasks":
            section = bundle_trial_tasks(directory, key_path(args.key_file), load(args.authors))
            print(f"Sealed {len(section['tasks'])} trial-task instances ({section['tree']['files']} files) into "
                  f"{TRIAL_TASK_CIPHERTEXT.relative_to(ROOT)}; delete the authoring directory once the commit lands")
        return
    if args.command == "seal":
        seal(args.authoring, key_path(args.key_file), load(args.authors))
    elif args.command == "seal-lookups":
        seal_lookups(args.authoring, key_path(args.key_file), load(args.authors))
    elif args.command == "verify":
        available = args.key_file or os.environ.get("INVESTIGATION_SEALED_KEY") or DEFAULT_KEY.expanduser().is_file()
        if args.public or not available:
            _, manifest, twins = verify_public()
            print(f"Verified {len(manifest['families'])} sealed family commitments against "
                  f"{manifest['preregistration']['source']} without opening the seal: "
                  + ", ".join(f"{d} {t} {n}" for (d, t), n in sorted(twins.items()))
                  + lookup_summary(manifest))
        else:
            manifest = verify_sealed(key_path(args.key_file))
            print(f"Opened the seal privately and verified {len(manifest['families'])} families"
                  f"{lookup_summary(manifest)}, {manifest['bundle']['plaintext_files']} files, "
                  "every committed digest and rubric.")
    else:
        destination = args.destination.resolve()
        if destination.is_relative_to(ROOT.resolve()):
            raise SystemExit("Open the seal outside the repository")
        destination.mkdir(mode=0o700)
        unpack(key_path(args.key_file), destination)
        print(f"Opened into {destination}")


if __name__ == "__main__":
    main()
