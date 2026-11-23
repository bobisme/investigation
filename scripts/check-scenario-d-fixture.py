#!/usr/bin/env python3
"""Mechanically evaluate the Scenario D fixture: digests, selectors, origins, search coverage, injection."""

import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FAMILY = ROOT / "fixtures/development/scenario-d-source-research"
VARIANTS = ("baseline", "later-edition-twin", "injected-output")


def flat(text):
    return " ".join(text.split())


def check_variant(variant):
    base = FAMILY / variant
    manifest = json.loads((base / "manifest.json").read_text(encoding="utf-8"))
    expected = json.loads((base / "expected.json").read_text(encoding="utf-8"))
    texts = {}
    for source in manifest["source_files"]:
        data = (base / source["path"]).read_bytes()
        assert hashlib.sha256(data).hexdigest() == source["sha256"], f"{variant}: digest {source['path']}"
        assert len(data) == source["bytes"], f"{variant}: size {source['path']}"
        texts[Path(source["path"]).name] = flat(data.decode("utf-8"))
    origins = {origin["id"]: origin for origin in manifest["claim_origins"]}
    for origin in origins.values():
        for passage in origin["passages"]:
            assert flat(passage["selector"]) in texts[passage["source"]], f"{variant}: origin selector {passage}"
    props = {prop["id"]: prop for prop in expected["properties"]}
    one = props["one_origin_for_disputed_value"]
    shared = origins[one["claim_origin"]]
    assert one["origin_count"] == 1
    assert sorted({p["source"] for p in shared["passages"]}) == sorted(one["acquisition_sources"])
    assert one["acquisitions_kept_as_separate_records"] == len(one["acquisition_sources"]) == 3
    bench = props["bench_test_separate_origin"]
    assert bench["claim_origin"] != bench["different_from"] and bench["claim_origin"] in origins
    for quote in props["every_original_selector_kept"]["quotes"]:
        assert flat(quote["quote"]) in texts[quote["source"]], f"{variant}: quote {quote}"
    erratum = props["erratum_currency"]
    twin = variant == "later-edition-twin"
    assert erratum["applies"] is (not twin)
    named = manifest["build_to_edition"]["named_build_edition"]
    assert erratum["named_build_edition"] == named
    applies_text = f"Affected edition: Edition {named}"
    assert (applies_text in texts["first-party-erratum.txt"]) is (not twin)
    coverage = json.loads((base / "sources/search-coverage-input.json").read_text(encoding="utf-8"))
    registries = {s["registry"] for s in coverage["searches"]}
    cover = props["search_coverage_includes_empty_results"]
    assert set(cover["registries"]) == registries, f"{variant}: registries"
    zero = {s["registry"] for s in coverage["searches"] if s["result_count"] == 0}
    assert set(cover["zero_result_registries"]) <= zero
    for search in coverage["searches"]:
        assert search["result_count"] == len(search["hits"])
    injected = "injected_text_inert" in props
    assert injected is (variant == "injected-output")
    if injected:
        text = texts["crawl-output-forum-sweep.txt"]
        for quote in props["injected_text_inert"]["quotes"]:
            assert flat(quote) in text
        assert "SYNTHETIC FIXTURE" in text and ".invalid" in text
        assert manifest["injected_instruction_samples"][0]["synthetic"] is True
        assert props["injected_text_inert"]["result_must_equal"] == "baseline"
    else:
        assert "crawl-output-forum-sweep.txt" not in texts
    return len(manifest["source_files"])


def check_digest_list():
    listing = json.loads((FAMILY / "digests.json").read_text(encoding="utf-8"))
    for item in listing["files"]:
        data = (FAMILY / item["path"]).read_bytes()
        assert hashlib.sha256(data).hexdigest() == item["sha256"] and len(data) == item["bytes"], item["path"]
    on_disk = {str(p.relative_to(FAMILY)) for p in FAMILY.rglob("*") if p.is_file() and p.name != "digests.json"}
    assert on_disk == {item["path"] for item in listing["files"]}


def check_twin_relationship():
    base, twin = (FAMILY / v / "sources" for v in ("baseline", "later-edition-twin"))
    for name in ("primary-specification.txt", "blog-post.txt"):
        assert (base / name).read_bytes() == (twin / name).read_bytes(), name
    inj, baseline = FAMILY / "injected-output/sources", FAMILY / "baseline/sources"
    for name in ("primary-specification.txt", "trade-article.txt", "blog-post.txt", "first-party-erratum.txt"):
        assert (inj / name).read_bytes() == (baseline / name).read_bytes(), name


if __name__ == "__main__":
    counts = [check_variant(v) for v in VARIANTS]
    check_digest_list()
    check_twin_relationship()
    print(f"Verified Scenario D: {len(VARIANTS)} variants, {sum(counts)} sources.")
