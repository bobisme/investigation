#!/usr/bin/env python3
"""Produce arm-blinded successor inputs, then apply frozen rubric clauses."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import random
import re
import uuid


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
IDENTIFIER = re.compile(r"\b(?:art|src|obs|case|crit|cmp|dec|concl|hyp|asm|cmp|run|inq|opt|assessment|rev|tx|review|retro)_[a-z0-9-]+\b", re.I)
SHA = re.compile(r"\b(?:sha256:)?[a-f0-9]{64}\b", re.I)
TOOL_NAME = re.compile(r"\b(?:investigation\s+(?:cli|workspace|casebook|command)|investigation --[a-z-]+|inv\s+(?:brief|show|evidence|source|record|next)|notes\.md|markdown notes|case journal|sqlite index)\b", re.I)
DOCS = ("source.txt", "update.txt")


def read(path):
    return json.loads(path.read_text(encoding="utf-8"))


def write(path, value):
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2, ensure_ascii=False)
        output.write("\n")


def normalized_words(text):
    return " ".join(re.findall(r"[\w]+", text.casefold()))


def mappings(workspace, documents):
    aliases = {}
    digests = {hashlib.sha256(data).hexdigest(): name for name, data in documents.items()}

    def visit(value):
        if isinstance(value, list):
            for element in value:
                visit(element)
        if not isinstance(value, dict):
            return
        header, body = value.get("header"), value.get("body")
        if isinstance(header, dict) and isinstance(body, dict):
            digest = body.get("digest", "")
            if isinstance(digest, str) and digest.removeprefix("sha256:") in digests:
                doc = digests[digest.removeprefix("sha256:")]
                identifier = header.get("id")
                if isinstance(identifier, str):
                    aliases[identifier] = doc
                source = body.get("source")
                if isinstance(source, dict) and isinstance(source.get("id"), str):
                    aliases[source["id"]] = doc
        for child in value.values():
            visit(child)

    for file in sorted(workspace.rglob("state/cases/*/events/*.json")):
        if file.is_file() and not file.is_symlink():
            visit(read(file))
    aliases.update(digests)
    aliases.update({f"sha256:{key}": value for key, value in digests.items()})
    return aliases


def select_citations(citation, documents, aliases):
    for alias, filename in sorted(aliases.items(), key=lambda row: -len(row[0])):
        citation = citation.replace(alias, filename)
    cited = {}
    matches = list(re.finditer(r"\b(?:source|update)\.txt\b", citation, re.I))
    for index, occurrence in enumerate(matches):
        name = occurrence.group().lower()
        if name not in documents:
            continue
        end = matches[index + 1].start() if index + 1 < len(matches) else len(citation)
        section = citation[occurrence.end():end]
        lines = documents[name].decode("utf-8").splitlines()
        for location in re.finditer(r"(?:#L|:\s*|\blines?\s+)(\d+)(?:\s*(?:[-–]|to)\s*L?(\d+))?", section, re.I):
            start = int(location.group(1))
            stop = int(location.group(2) or start)
            if 1 <= start <= stop <= len(lines) and stop - start <= 2:
                cited.setdefault(name, set()).update(range(start, stop + 1))
        excerpt = normalized_words(section)
        for number, line in enumerate(lines, 1):
            words = normalized_words(line).split()
            if len(words) < 5 or not excerpt:
                continue
            if any(" ".join(words[i:i + 5]) in excerpt for i in range(len(words) - 4)):
                cited.setdefault(name, set()).add(number)
    return {name: sorted(numbers) for name, numbers in sorted(cited.items())}


def clean_field(text):
    text = IDENTIFIER.sub("evidence reference", text)
    text = SHA.sub("source digest", text)
    text = TOOL_NAME.sub("working record", text)
    text = re.sub(r"```(?:json|text|markdown)?|`|\*\*|__|^\s*#+\s*", "", text, flags=re.M)
    return " ".join(text.split())


def blind(archive, output, seed):
    manifest = read(archive / "manifest.json")
    expected = {
        (item["family"], item["variant"], item["model"], arm)
        for item in manifest["blocks"] for arm in item["order"]
    }
    registry = read(FIXTURES / "family-registry.json")
    variant_registry = {(family["id"], variant["id"]): variant for family in registry["families"]
                        for variant in family["variants"]}
    rows = []
    mapping = {}
    actual = set()
    seen_sessions = set()
    for file in sorted(archive.glob("*/run.json")):
        run = read(file)
        a = run["assignment"]
        match = (a["family"], a["variant"], run["environment"]["model"], a["arm"])
        if match in actual or match not in expected:
            raise ValueError(f"Duplicate or unexpected assignment: {match}")
        actual.add(match)
        for phase in run["phases"].values():
            session = phase["session_id"]
            if session in seen_sessions:
                raise ValueError(f"Reused phase session: {session}")
            seen_sessions.add(session)
        variant = variant_registry[match[:2]]
        documents = {name: (FIXTURES / variant["directory"] / name).read_bytes()
                     for name in DOCS if (FIXTURES / variant["directory"] / name).is_file()}
        answer = read(file.with_name("answer.json"))
        parsed = None
        citations = {}
        if answer is not None:
            aliases = mappings(file.parent / "workspace", documents)
            citations = select_citations(answer["citation"], documents, aliases)
            parsed = {field: clean_field(answer[field]) for field in ("answer", "limitation", "next")}
        response_id = str(uuid.uuid4())
        rows.append({"id": response_id, "family": match[0], "variant": match[1],
                     "answer": parsed, "citations": citations})
        mapping[response_id] = dict(a, model=match[2], source_run=file.parent.name)
    if actual != expected:
        raise ValueError(f"Missing outcome records: {sorted(expected - actual)}")
    random.Random(seed).shuffle(rows)
    output.mkdir(mode=0o700)
    write(output / "blinded.json", rows)
    write(output / "assignment-map.json", mapping)


def matches(field, clauses):
    return all(re.search(clause, field, re.I) for clause in clauses)


def score(row, rubric):
    answer = row["answer"] or {"answer": "", "limitation": "", "next": ""}
    correct = {}
    for item in rubric["warranted"]:
        if "citation" in item:
            document = item["citation"]["document"]
            correct[item["id"]] = bool(set(item["citation"]["any_lines"]) & set(row["citations"].get(document, [])))
        else:
            field = answer[item["field"]]
            correct[item["id"]] = bool(field) and matches(field, item["all"])
    excess = {}
    for item in rubric["false_promotions"]:
        # A condition elsewhere in the answer should not excuse a declarative
        # overclaim. Evaluate sentences or clauses independently.
        sentences = re.split(r"(?<=[.!?;])\s+", answer[item["field"]])
        excess[item["id"]] = any(
            matches(sentence, item["all"]) and not any(re.search(pattern, sentence, re.I)
                                                       for pattern in item.get("unless", []))
            for sentence in sentences if sentence
        )
    return {
        "id": row["id"], "correct": correct, "unwarranted": excess,
        "accuracy": sum(correct.values()) / len(correct),
        "false_promotion_rate": sum(excess.values()) / len(excess),
        "missed_warranted": len(correct) - sum(correct.values()),
    }


def grade(blinded, key_root, output):
    rows = read(blinded)
    keys = {}
    scored = []
    ids = set()
    for row in rows:
        family = row["family"]
        variant = row["variant"]
        if family not in keys:
            keys[family] = read(key_root / f"{family}.json")
        if row["id"] in ids:
            raise ValueError(f'Duplicate blinded ID {row["id"]}')
        ids.add(row["id"])
        scored.append(score(row, keys[family]["variants"][variant]))
    write(output, scored)


def self_test():
    documents = {"source.txt": b"Oranges grow in the larger orchard.\nApple trees produce a harvest.\n",
                 "update.txt": b"Only roads shorter than five miles qualify.\nThe west route is eight miles long.\n"}
    synthetic = {"art_1234-5678": "update.txt"}
    for text in ("update.txt:2", "update.txt lines 2-2", "art_1234-5678 #L2",
                 "update.txt: The west route is eight miles long"):
        assert select_citations(text, documents, synthetic) == {"update.txt": [2]}, text
    assert select_citations("update.txt lines 1–10", documents, {}) == {}
    assert select_citations("update.txt", documents, {}) == {}
    assert "inv brief" not in clean_field("According to `inv brief` and art_1234-5678.")
    rubric = {
        "warranted": [{"id": "answer", "field": "answer", "all": ["west", "ineligible"]},
                      {"id": "citation", "citation": {"document": "update.txt", "any_lines": [2]}}],
        "false_promotions": [{"id": "early", "field": "answer", "all": ["west route is eligible"],
                              "unless": ["hypothetical", "not"]}],
    }
    row = {"id": "x", "answer": {"answer": "West is ineligible", "limitation": "-", "next": "-"},
           "citations": {"update.txt": [2]}}
    assert score(row, rubric)["accuracy"] == 1
    assert score(row, rubric)["false_promotion_rate"] == 0
    row["answer"] = {"answer": "West route is eligible", "limitation": "-", "next": "-"}
    assert score(row, rubric)["missed_warranted"] == 1
    assert score(row, rubric)["false_promotion_rate"] == 1
    row["answer"] = None
    row["citations"] = {}
    assert score(row, rubric)["missed_warranted"] == 2
    assert score(row, rubric)["false_promotion_rate"] == 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("blind")
    prepare.add_argument("archive", type=Path)
    prepare.add_argument("output", type=Path)
    prepare.add_argument("--seed", type=int, default=8103)
    scoring = commands.add_parser("grade")
    scoring.add_argument("blinded", type=Path)
    scoring.add_argument("keys", type=Path)
    scoring.add_argument("output", type=Path)
    commands.add_parser("self-test")
    args = parser.parse_args()
    if args.command == "blind":
        blind(args.archive, args.output, args.seed)
    elif args.command == "grade":
        grade(args.blinded, args.keys, args.output)
    else:
        self_test()
        print("Blinded grading and citation normalization checks passed.")
