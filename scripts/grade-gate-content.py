#!/usr/bin/env python3
"""Gate-content grading (W14.06, plan §20.5): strawman-rival rate and checkable-retrospective rate.

Gate presence is not gate content. A gate that asks for a rival can be satisfied by a rival nobody
believes, and one that asks for a retrospective can be satisfied by a platitude. This tool grades the
content from the verified case journal, never from prose. Definitions, denominators and limits are in
docs/gate-content.md; the thresholds below are the committed rubric (RUBRIC_VERSION).

  grade      grade one `inv`-arm workspace against a key's `rivals`; print one JSON result
  summarize  aggregate graded results per agent family with counts, denominators and Wilson intervals
  sample     draw a deterministic sample of graded units for hand review
  rubric     print the committed rubric constants
  rehearse   build good and strawman runs per metric and show the metrics tell them apart

Stdlib only. The grader is deterministic text and structure matching; it can miss paraphrases, so hand
review a sample and report the raw counts (docs/gate-content.md, "Limits").
"""

import argparse
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import re
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
KEYS = ROOT / "training/gate-content/keys"
RUBRIC_VERSION = "gate-content.v1"

# --- The committed rubric ------------------------------------------------------------------------

MATCH_MIN_SHARED = 3          # content tokens a recorded rival shares with a key rival
MATCH_MIN_FRACTION = 0.5      # of the shorter side's content tokens
PREDICTION_MIN_TOKENS = 4     # content tokens in a prediction, expectation rationale or condition
RESTATEMENT_MAX_OVERLAP = 0.8 # a prediction that repeats the rival's own statement predicts nothing
KEY_AGREE_MIN_SHARED = 2
KEY_AGREE_MIN_FRACTION = 0.4
TRIGGER_MIN_TOKENS = 4
TRIGGER_MIN_SPECIFIC = 2      # content tokens outside GENERIC
ACTION_MIN_TOKENS = 4

STOP = set("""a an the and or but of to in on at for from by with without as is are was were be been being it its
this that these those there their then than so such not no nor if when whenever while after before once unless
where which who whom what why how do does did done can could should would may might must will shall has have had
into onto over under about between within also only just very more most less any all each every other another
one two itself themselves himself herself because""".split())
GENERIC = set("""case cases investigation investigations problem problems issue issues thing things work evidence data
result results record records note notes step steps check checks time way ways make makes use used using
conclusion conclusions question questions hypothesis hypotheses""".split())
ACTION_VERBS = set("""check compare isolate measure record rerun repeat re-run test verify ask run read log capture
inspect swap replace disable enable skip move reschedule request pull gather collect diff bisect reproduce confirm
rule exclude schedule instrument sample ping query review open contact fetch extract compute calibrate recalibrate
add attach cite name list state write note re-check retest reread""".split())
VAGUE = re.compile(
    r"\b(?:be (?:careful|thorough|vigilant|diligent|rigorous)|pay (?:close )?attention|keep in mind|stay (?:curious|open)"
    r"|think (?:carefully|critically)|use (?:good )?judg(?:e)?ment|communicate (?:well|clearly)|remember to be"
    r"|double[- ]check everything|always verify|trust (?:your )?(?:instincts|gut)|be mindful|stay focused)\b", re.I)
GENERIC_TRIGGER = re.compile(
    r"^\s*(?:always|in general|generally|in any|in every|in all|any|every|all|whenever (?:you|we|one) (?:investigate|work|debug|research))\b"
    r"|\b(?:any|every|all) (?:investigations?|cases?|problems?|tasks?|work)\b", re.I)
CONDITIONAL = re.compile(r"\b(?:when|if|whenever|once|after|before|while|where|unless|on)\b", re.I)
PRAISE = re.compile(
    r"\b(?:great|excellent|well done|perfect(?:ly)?|flawless(?:ly)?|smooth(?:ly)?|worked (?:very )?well|successful(?:ly)?"
    r"|impressive(?:ly)?|good job|went well|exemplary|nailed)\b", re.I)
DEAD = {"retired", "retracted", "withdrawn", "superseded", "rejected"}  # `rejected_within_scope`/`ruled_out` stay: assessed


def rubric():
    return {"version": RUBRIC_VERSION, **{name: value for name, value in sorted(globals().items())
            if re.fullmatch(r"[A-Z_]+", name) and isinstance(value, (int, float)) and name != "RUBRIC_VERSION"}}


# --- Text ----------------------------------------------------------------------------------------


def tokens(text):
    """Content tokens: casefolded, stop words dropped, a trailing plural `s` removed."""
    found = set()
    for word in re.findall(r"[\w][\w.:-]*", str(text).casefold()):
        word = word.strip(".:-")
        if len(word) < 3 and not any(c.isdigit() for c in word):
            continue
        if word in STOP:
            continue
        if len(word) > 4 and word.endswith("s") and not word.endswith("ss"):
            word = word[:-1]
        found.add(word)
    return found


def overlap(left, right):
    shared = len(left & right)
    smaller = min(len(left), len(right))
    return shared, (shared / smaller if smaller else 0.0)


def matches(text, key_text, min_shared, min_fraction):
    shared, fraction = overlap(tokens(text), tokens(key_text))
    return shared >= min_shared and fraction >= min_fraction


def wilson(k, n, z=1.96):
    if n == 0:
        return None
    p = k / n
    d = 1 + z * z / n
    centre = (p + z * z / (2 * n)) / d
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return [round(max(0.0, centre - half), 4), round(min(1.0, centre + half), 4)]


# --- Journal (the same verified read as scripts/trial-tasks.py) ---------------------------------


def trial_tasks():
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("trial_tasks", ROOT / "scripts/trial-tasks.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def kind(identifier):
    return identifier.split("_", 1)[0]


def current_records(journal, prefix, after=None):
    """The current revision of every record of one kind written after the per-case sequence marks."""
    after = after or {}
    found = []
    for identifier, revisions in journal.revisions.items():
        if kind(identifier) != prefix:
            continue
        latest = revisions[-1]
        first = revisions[0]
        if first["_sequence"] <= after.get(latest["header"]["case_id"], 0):
            continue
        found.append(latest)
    return sorted(found, key=lambda item: (item["_sequence"], item["header"]["id"]))


def lifecycle(item):
    return item["body"].get("lifecycle")


def status_dead(item):
    body = item["body"]
    return lifecycle(item) in DEAD or body.get("status") == "retracted" or body.get("retracted") is True


# --- Strawman rivals -----------------------------------------------------------------------------


def key_rivals(key):
    rivals = key.get("rivals")
    if not isinstance(rivals, list) or not rivals:
        raise SystemExit("the key has no `rivals` list of {explanation, distinguishing_prediction}")
    return rivals


def plan_predictions(journal, after):
    """Per live confirmatory plan: {hypothesis id: [predicted outcome]}."""
    plans = []
    for plan in current_records(journal, "inq", after):
        inquiry = plan["body"].get("inquiry") or {}
        if status_dead(plan) or lifecycle(plan) == "retired" or inquiry.get("mode") != "confirmatory":
            continue
        by_rival = {}
        for prediction in (inquiry.get("confirmatory") or {}).get("predictions", []):
            by_rival.setdefault(prediction["rival_ref"]["id"], []).append(str(prediction.get("predicted_outcome", "")))
        plans.append(by_rival)
    return plans


def hypothesis_prediction(identifier, statement, plans, assessments):
    """The distinguishing prediction recorded for one hypothesis, as (kind, text) or None.

    A plan prediction counts when the plan predicts for two or more hypotheses, the text has enough
    content, does not repeat the hypothesis's own statement and differs from every other hypothesis's
    prediction in that plan. An assessment counts when it records `more_expected` or `less_expected`
    for the hypothesis with a substantive rationale."""
    for plan in plans:
        if identifier not in plan or len(plan) < 2:
            continue
        others = {normalize(text) for other, texts in plan.items() if other != identifier for text in texts}
        for text in plan[identifier]:
            if len(tokens(text)) < PREDICTION_MIN_TOKENS or normalize(text) in others:
                continue
            if overlap_fraction(tokens(text), tokens(statement)) > RESTATEMENT_MAX_OVERLAP:
                continue
            return "plan_prediction", text
    for assessment in assessments:
        for expectation in assessment["body"].get("rival_expectations") or []:
            if (expectation["rival_ref"]["id"] == identifier and expectation.get("expectation") in ("more_expected", "less_expected")
                    and len(tokens(expectation.get("rationale", ""))) >= PREDICTION_MIN_TOKENS):
                return "assessed_expectation", expectation["rationale"]
    return None


def normalize(text):
    return " ".join(re.findall(r"[\w.]+", str(text).casefold()))


def overlap_fraction(small, large):
    return len(small & large) / len(small) if small else 0.0


def option_prediction(identifier, decisions):
    """A Decision's reversal condition for the option: the condition under which it would be preferred."""
    for decision in decisions:
        conditions = decision["body"].get("reversal_conditions") or []
        others = {normalize(c.get("condition", "")) for c in conditions if c["option_ref"]["id"] != identifier}
        for condition in conditions:
            text = condition.get("condition", "")
            if condition["option_ref"]["id"] == identifier and len(tokens(text)) >= PREDICTION_MIN_TOKENS \
                    and normalize(text) not in others:
                return "reversal_condition", text
    return None


def grade_strawman(journal, key, after):
    rivals = key_rivals(key)
    plans = plan_predictions(journal, after)
    assessments = [a for a in current_records(journal, "assess", after) if not status_dead(a)]
    decisions = [d for d in current_records(journal, "dec", after) if not status_dead(d)]
    recorded = []
    for item in current_records(journal, "hyp", after):
        if not status_dead(item):
            recorded.append(("hypothesis", item, item["body"].get("statement", "")))
    for item in current_records(journal, "opt", after):
        if not status_dead(item):
            recorded.append(("option", item, item["body"].get("description", "")))
    units, claimed = [], {}
    for what, item, text in recorded:
        identifier = item["header"]["id"]
        scores = [overlap(tokens(text), tokens(rival["explanation"])) for rival in rivals]
        best = max(range(len(rivals)), key=lambda i: (scores[i][1], scores[i][0]))
        matched = matches(text, rivals[best]["explanation"], MATCH_MIN_SHARED, MATCH_MIN_FRACTION)
        found = hypothesis_prediction(identifier, text, plans, assessments) if what == "hypothesis" \
            else option_prediction(identifier, decisions)
        reasons = []
        duplicate = False
        if not matched:
            reasons.append("no_key_rival")
        else:
            duplicate = best in claimed
            claimed.setdefault(best, identifier)
            if duplicate:
                reasons.append("duplicate_of_key_rival")
        if not found:
            reasons.append("no_distinguishing_prediction")
        agrees = bool(found and matched and matches(found[1], rivals[best]["distinguishing_prediction"],
                                                    KEY_AGREE_MIN_SHARED, KEY_AGREE_MIN_FRACTION))
        units.append({"id": identifier, "case": item["header"]["case_id"], "kind": what, "text": text,
                      "key_rival": best if matched else None, "prediction": found[1] if found else None,
                      "prediction_source": found[0] if found else None, "prediction_agrees_with_key": agrees,
                      "strawman": bool(reasons), "reasons": reasons})
    count = lambda reason: sum(reason in unit["reasons"] for unit in units)
    covered = sorted({unit["key_rival"] for unit in units if unit["key_rival"] is not None and not unit["strawman"]})
    return {"denominator": len(units), "strawman": sum(unit["strawman"] for unit in units),
            "by_reason": {"no_key_rival": count("no_key_rival"), "duplicate_of_key_rival": count("duplicate_of_key_rival"),
                          "no_distinguishing_prediction": count("no_distinguishing_prediction")},
            "key_rivals": len(rivals), "key_rivals_covered": len(covered),
            "prediction_agrees_with_key": sum(unit["prediction_agrees_with_key"] for unit in units), "units": units}


# --- Checkable retrospectives --------------------------------------------------------------------


def lesson_checks(lesson, journal, case):
    trigger, guidance = lesson.get("applies_when", ""), lesson.get("guidance", "")
    trigger_tokens = tokens(trigger)
    verbs = [word for word in re.findall(r"[a-z][a-z-]*", guidance.casefold())][:4]
    refs = lesson.get("support_refs") or []
    resolves = [journal.revisions.get(ref, [None])[0] for ref in refs]
    checks = {
        "trigger_is_conditional": bool(CONDITIONAL.search(trigger)),
        "trigger_is_specific": len(trigger_tokens) >= TRIGGER_MIN_TOKENS
                               and len(trigger_tokens - GENERIC) >= TRIGGER_MIN_SPECIFIC
                               and not GENERIC_TRIGGER.search(trigger),
        "action_is_concrete": len(tokens(guidance)) >= ACTION_MIN_TOKENS and not VAGUE.search(guidance)
                              and any(word in ACTION_VERBS for word in verbs),
        "cites_case_events": bool(refs) and all(r is not None and r["header"]["case_id"] == case for r in resolves),
        "no_praise": not PRAISE.search(guidance) and not PRAISE.search(trigger),
    }
    return checks


def grade_retrospectives(journal, after):
    units, retros, empty_with_reason, steps, unsupported = [], 0, 0, 0, 0
    for retro in current_records(journal, "retro", after):
        if status_dead(retro):
            continue
        body, case = retro["body"], retro["header"]["case_id"]
        retros += 1
        lessons = body.get("lessons") or []
        if not lessons and body.get("no_reusable_lesson_reason"):
            empty_with_reason += 1
        for step in body.get("invalid_or_misleading_steps") or []:
            steps += 1
            refs = step.get("support_refs") or []
            if not refs or any(journal.revisions.get(ref) is None for ref in refs):
                unsupported += 1
        for index, lesson in enumerate(lessons):
            checks = lesson_checks(lesson, journal, case)
            units.append({"id": f"{retro['header']['id']}:{index}", "case": case, "applies_when": lesson.get("applies_when"),
                          "guidance": lesson.get("guidance"), "checks": checks, "checkable": all(checks.values()),
                          "failed": sorted(name for name, ok in checks.items() if not ok)})
    failed = {}
    for unit in units:
        for name in unit["failed"]:
            failed[name] = failed.get(name, 0) + 1
    return {"denominator": len(units), "checkable": sum(unit["checkable"] for unit in units),
            "retrospectives": retros, "retrospectives_without_lesson_and_with_reason": empty_with_reason,
            "failed_by_check": dict(sorted(failed.items())),
            "invalid_step_denominator": steps, "invalid_steps_without_resolving_support": unsupported, "units": units}


# --- Commands ------------------------------------------------------------------------------------


def load_key(name):
    path = Path(name)
    if not path.is_file():
        path = KEYS / f"{name}.json"
    return json.loads(path.read_text(encoding="utf-8"))


def grade_workspace(binary, workspace, key, agent_family="unlabelled", after=None, run_id=None):
    tt = trial_tasks()
    journal = tt.Journal(binary, workspace)  # runs `investigation check` first and aborts if it fails
    return {"schema_version": "gate-content-grade.v1", "rubric": RUBRIC_VERSION, "run": run_id or str(workspace),
            "agent_family": agent_family, "key_family": key.get("family"), "journal_events": len(journal.events),
            "strawman_rivals": grade_strawman(journal, key, after or {}),
            "retrospectives": grade_retrospectives(journal, after or {})}


def rate(k, n):
    return {"k": k, "n": n, "rate": round(k / n, 4) if n else None, "wilson95": wilson(k, n)}


def summarize(results):
    groups = {}
    for result in results:
        groups.setdefault(result["agent_family"], []).append(result)
    families = {}
    for name, runs in sorted(groups.items()):
        s = [r["strawman_rivals"] for r in runs]
        t = [r["retrospectives"] for r in runs]
        families[name] = {
            "runs": len(runs),
            "strawman_rival_rate": rate(sum(x["strawman"] for x in s), sum(x["denominator"] for x in s)),
            "runs_with_no_recorded_rival": sum(x["denominator"] == 0 for x in s),
            "checkable_retrospective_rate": rate(sum(x["checkable"] for x in t), sum(x["denominator"] for x in t)),
            "runs_with_no_lesson": sum(x["denominator"] == 0 for x in t),
            "key_rivals_covered": rate(sum(x["key_rivals_covered"] for x in s), sum(x["key_rivals"] for x in s)),
        }
    total_s = [r["strawman_rivals"] for r in results]
    total_t = [r["retrospectives"] for r in results]
    return {"schema_version": "gate-content-summary.v1", "rubric": RUBRIC_VERSION, "runs": len(results),
            "overall": {"strawman_rival_rate": rate(sum(x["strawman"] for x in total_s), sum(x["denominator"] for x in total_s)),
                        "checkable_retrospective_rate": rate(sum(x["checkable"] for x in total_t), sum(x["denominator"] for x in total_t))},
            "by_agent_family": families,
            "note": "A rate with n = 0 is undefined, not zero; read it with runs_with_no_recorded_rival and runs_with_no_lesson. "
                    "Runs of one family are correlated; the interval treats units as independent and is a lower bound on the uncertainty."}


def sample(results, seed, size):
    pool = []
    for result in results:
        for metric, section in (("strawman_rival", result["strawman_rivals"]), ("retrospective_lesson", result["retrospectives"])):
            for unit in section["units"]:
                pool.append({"run": result["run"], "agent_family": result["agent_family"], "metric": metric, "unit": unit})
    pool.sort(key=lambda row: hashlib.sha256(f"{seed}|{row['run']}|{row['metric']}|{row['unit']['id']}".encode()).hexdigest())
    return pool[:size]


# --- Rehearsal -----------------------------------------------------------------------------------


def exact(item):
    return {"id": item["id"], "revision": item["revision"]}


class Builder:
    """One synthetic `inv`-arm run, written through the public binary."""

    def __init__(self, tt, binary, base, family):
        self.cli = tt.Cli(binary, tempfile.mkdtemp(prefix=f"gate-{family}-", dir=base))
        tt.init_workspace(self.cli)
        (self.cli.root / "source.txt").write_text("synthetic source\n", encoding="utf-8")
        started = self.cli.json(["start", "Why does the symptom occur?", "--attach", "source.txt", "--command-id", "g-start"], actor="agent:rehearsal")
        self.case = started["created"][0]["id"]
        self.count = 0
        created = self.record([{"op": "question.create", "client_ref": "q",
                                "payload": {"wording": "What explains the symptom?", "importance": "high"}}])
        self.question = exact(created[0])

    def record(self, operations):
        self.count += 1
        envelope = {"schema_version": "inv.command.v1", "command_id": f"g-{self.count}", "case_id": self.case,
                    "actor": "agent:rehearsal", "preconditions": [], "operations": operations}
        response = self.cli.json(["record", "--input", "-"], envelope, actor="agent:rehearsal")
        assert response["result"] == "committed", response
        return response["created"]

    def hypotheses(self, statements):
        ops = [{"op": "hypothesis.create", "client_ref": f"h{i}", "payload": {
            "question_ref": self.question, "statement": text, "possible_falsifiers": ["A recorded observation contradicts it."],
            "rival_refs": [], "assumption_refs": []}} for i, text in enumerate(statements)]
        return [exact(item) for item in self.record(ops)]

    def options(self, descriptions):
        ops = [{"op": "option.create", "client_ref": f"o{i}", "payload": {
            "question_ref": self.question, "description": text, "dependencies": [], "tradeoff_notes": ["Costs differ."]}}
            for i, text in enumerate(descriptions)]
        return [exact(item) for item in self.record(ops)]

    def plan(self, hypotheses, outcomes):
        predictions = [{"rival_ref": h, "predicted_outcome": o} for h, o in zip(hypotheses, outcomes)]
        self.record([{"op": "inquiry.create", "client_ref": "p", "payload": {
            "purpose": "Tell the accounts apart.", "method": "Compare the recorded outcomes.", "controls": [], "limits": [],
            "desired_observations": [], "inquiry": {"mode": "confirmatory", "confirmatory": {
                "predictions": predictions, "evidence_holdout_boundary": "Nothing was read before the plan.",
                "prior_exposure": "The source text only.", "stop_analysis_rule": "Stop after one pass."}}}}])

    def decision(self, options, conditions):
        assumption = exact(self.record([{"op": "assumption.create", "client_ref": "a", "payload": {
            "claim": "The workload matches the one described in the source.", "consequence": "The options would rank differently.",
            "materiality": "material", "affected_refs": options}}])[0])
        self.record([{"op": "decision.propose", "client_ref": "d", "payload": {
            "question_ref": self.question, "choice": {"kind": "select", "option_ref": options[0]},
            "compared_alternatives": options, "comparison_refs": [], "assumption_refs": [assumption], "evidence_refs": [],
            "consequences": ["The selected option is the working choice."], "tradeoffs": ["Each option trades durability against latency."], "hard_conditions": [], "check_when": None,
            "expected_outcome": "The choice holds.", "reopen_trigger": "A listed condition occurs.",
            "reversal_conditions": [{"option_ref": o, "condition": c, "assumption_ref": assumption} for o, c in zip(options, conditions)]}}])

    def retrospective(self, lessons, reason=None):
        draft = self.cli.json(["--case", self.case, "retrospective", "--draft"], actor="agent:rehearsal")
        cite = self.question["id"]
        full = [dict(lesson, support_refs=[cite] if lesson.get("support_refs") is None else lesson["support_refs"],
                     transfer_limits=lesson.get("transfer_limits", "One synthetic case.")) for lesson in lessons]
        self.record([{"op": "retrospective.record", "payload": {
            "case_ref": {"id": self.case, "revision": 1}, "snapshot": draft["snapshot"],
            "methods_attempted": [{"name": "Compare the accounts against the source."}],
            "effective_steps": [], "invalid_or_misleading_steps": [],
            "none_observed_reason": "No step misled the work in this synthetic case.", "costs_or_constraints": "None recorded.",
            "residual_unknowns": [], "lessons": full,
            **({"no_reusable_lesson_reason": reason} if reason else {})}}])


GOOD_RIVALS = {
    "tide-gauge-dropout": (
        ["Pump P1's 02:10 start dips the shared supply and the gauge's sensor head stops replying.",
         "Modem M-2's firmware check at 02:10 draws current from the shared supply or holds the gauge's serial line.",
         "A scheduled task on the logger pauses sampling at 02:10, independent of the pump and the modem."],
        ["The dropouts stop on nights when pump P1 does not start.",
         "The dropouts continue with pump P1 isolated and shorten when the modem firmware check is moved.",
         "The dropouts begin at 02:10 with pump and modem check disabled, and the logger task log shows a job."],
        ["The gauge is cursed by cosmic rays at night.", "The tide itself changes the sampling rate at 02:10.",
         "Somebody unplugs the gauge for fun."]),
    "door-alarm-delay": (
        ["Firmware 5.2 uses a 45 second door-open alarm delay as service bulletin SB-12 says.",
         "Firmware 5.2 still uses a 90 second door-open alarm delay as datasheet revision C states.",
         "The alarm delay is a site setting, so the observed 45 seconds is local and not the firmware default."],
        ["Door-open to alarm gaps on firmware 5.x controllers are about 45 seconds.",
         "Door-open to alarm gaps on a firmware 5.2 controller are about 90 seconds and SB-12 does not apply.",
         "Controllers in other rooms or after a settings reset show a different delay on the settings screen."],
        ["The door is haunted by a ghost.", "The datasheet was written by an unreliable narrator.",
         "The alarm is powered by magic."]),
    "proofer-temperature-drift": (
        ["Sensor S-A reads high in the afternoon because radiant heat warms its rear-wall bracket.",
         "The cabinet air is really warm in the afternoon and probe R-1 reads low from a fault or its placement.",
         "Sensor S-A has a temperature-dependent calibration drift in the sensor itself."],
        ["Moving S-A to shelf height removes the afternoon offset, which follows the rear-wall temperature.",
         "A second calibrated probe at shelf height also reads about 30 C and R-1 fails a fresh bath check.",
         "S-A reads offset in a reference bath at afternoon temperature and the offset stays at shelf height."],
        ["Dough rises faster when watched.", "The log was typed by a tired clerk.", "Afternoons are inherently warmer than mornings."]),
}
GOOD_LESSONS = [
    {"applies_when": "When a nightly dropout continues while the suspected load is isolated and the extract lacks the other device's log",
     "guidance": "Request the other device's own log and compare its scheduled jobs with the dropout window before ranking accounts."},
    {"applies_when": "If a vendor figure is contradicted by a dated bulletin and only one controller was logged",
     "guidance": "Log the alarm gap on a second controller and cite both logs before recommending a setting."},
]
STRAW_LESSONS = [
    {"applies_when": "Always", "guidance": "Be careful and think carefully about every investigation."},
    {"applies_when": "In any case where there is a problem", "guidance": "Keep in mind that evidence matters."},
    {"applies_when": "When the investigation went smoothly", "guidance": "Repeat the excellent approach that worked very well."},
]


def rehearse(binary, base):
    tt = trial_tasks()
    checks, failures = [], []

    def expect(name, condition, detail=""):
        checks.append({"check": name, "ok": bool(condition)})
        if not condition:
            failures.append(f"{name}: {detail}")

    report = {}
    for family, (real, predictions, fillers) in GOOD_RIVALS.items():
        key = load_key(family)
        same = ["The outcome is observed."] * 3
        scenario = {}

        def run(label, hypotheses, outcomes, lessons=None):
            b = Builder(tt, binary, base, f"{family}-{label}")
            hs = b.hypotheses(hypotheses)
            if outcomes is not None:
                b.plan(hs, outcomes)
            if lessons is not None:
                b.retrospective(lessons)
            graded = grade_workspace(binary, b.cli.root, key, "rehearsal", run_id=f"{family}/{label}")
            scenario[label] = {"strawman": [graded["strawman_rivals"]["strawman"], graded["strawman_rivals"]["denominator"]],
                               "lessons": [graded["retrospectives"]["checkable"], graded["retrospectives"]["denominator"]]}
            return graded

        good = run("good", real, predictions, GOOD_LESSONS)
        s, t = good["strawman_rivals"], good["retrospectives"]
        expect(f"{family}: good run has no strawman rival", s["strawman"] == 0 and s["denominator"] == 3, json.dumps(s["units"])[:900])
        expect(f"{family}: good run covers every key rival", s["key_rivals_covered"] == s["key_rivals"] == 3)
        expect(f"{family}: good predictions agree with the key", s["prediction_agrees_with_key"] == 3, json.dumps(s["units"])[:900])
        expect(f"{family}: good lessons are all checkable", t["checkable"] == t["denominator"] == 2, json.dumps(t["units"])[:900])

        straw = run("strawman", fillers, ["It happens.", "It happens again.", "Something occurs."], STRAW_LESSONS)
        s, t = straw["strawman_rivals"], straw["retrospectives"]
        expect(f"{family}: strawman run is all strawman", s["strawman"] == s["denominator"] == 3 and s["by_reason"]["no_key_rival"] == 3,
               json.dumps(s["by_reason"]))
        expect(f"{family}: strawman lessons are none checkable", t["checkable"] == 0 and t["denominator"] == 3, json.dumps(t["units"])[:900])

        # One clause at a time: the real rivals with a shared prediction are strawmen for lacking a distinguishing prediction.
        shared = run("shared-prediction", real, same)
        s = shared["strawman_rivals"]
        expect(f"{family}: real rivals with one shared prediction are flagged", s["strawman"] == 3
               and s["by_reason"]["no_distinguishing_prediction"] == 3 and s["by_reason"]["no_key_rival"] == 0, json.dumps(s["by_reason"]))
        none = run("no-plan", real, None)
        expect(f"{family}: real rivals with no prediction at all are flagged",
               none["strawman_rivals"]["by_reason"]["no_distinguishing_prediction"] == 3)
        # A padded paraphrase of one real rival is not a second alternative.
        padded = run("padded", real + [real[0] + " (restated)"], predictions + ["Padding predicts something different here."])
        expect(f"{family}: a paraphrase of one real rival is a duplicate",
               padded["strawman_rivals"]["by_reason"]["duplicate_of_key_rival"] == 1, json.dumps(padded["strawman_rivals"]["by_reason"]))
        # Mixed: two real rivals and one filler gives 1 of 3.
        mixed = run("mixed", real[:2] + fillers[:1], predictions[:2] + ["It happens at some other time."])
        expect(f"{family}: two real and one filler gives 1/3", mixed["strawman_rivals"]["strawman"] == 1
               and mixed["strawman_rivals"]["denominator"] == 3, json.dumps(mixed["strawman_rivals"]["by_reason"]))
        # Lessons: each clause alone.
        for clause, lesson in (("trigger_is_specific", {"applies_when": "When any investigation has a problem", "guidance": GOOD_LESSONS[0]["guidance"]}),
                               ("action_is_concrete", {"applies_when": GOOD_LESSONS[0]["applies_when"], "guidance": "Be careful and think carefully."}),
                               ("no_praise", {"applies_when": GOOD_LESSONS[0]["applies_when"],
                                              "guidance": "Request the other device's own log; the excellent approach worked very well."}),
                               ("cites_case_events", dict(GOOD_LESSONS[0], support_refs=[]))):
            if clause == "cites_case_events":
                continue  # the store refuses a lesson without a same-case citation; the grader clause is exercised below
            graded = run(f"lesson-{clause}", real, predictions, [lesson])
            expect(f"{family}: a lesson failing only {clause} is not checkable",
                   graded["retrospectives"]["failed_by_check"] == {clause: 1}, json.dumps(graded["retrospectives"]["failed_by_check"]))
        empty = Builder(tt, binary, base, f"{family}-empty")
        graded = grade_workspace(binary, empty.cli.root, key, "rehearsal", run_id=f"{family}/empty")
        expect(f"{family}: a run with no rival or lesson has an undefined rate, not zero",
               graded["strawman_rivals"]["denominator"] == 0 and graded["retrospectives"]["denominator"] == 0
               and rate(0, 0)["rate"] is None)
        report[family] = scenario

    # The citation clause cannot be reached through the store (it refuses an uncited lesson), so test it on a stand-in journal.
    class Stub:
        revisions = {"q_1": [{"header": {"case_id": "case_1"}}]}
    lesson = dict(GOOD_LESSONS[0])
    expect("lesson: a resolving same-case citation passes", lesson_checks(dict(lesson, support_refs=["q_1"]), Stub, "case_1")["cites_case_events"])
    expect("lesson: no citation, an unknown record and another case's record all fail",
           not any(lesson_checks(dict(lesson, support_refs=refs), Stub, "case_1")["cites_case_events"]
                   for refs in ([], ["q_9"], ["q_9", "q_1"])) and
           not lesson_checks(dict(lesson, support_refs=["q_1"]), Stub, "case_2")["cites_case_events"])

    # Options: a Decision's reversal conditions are the distinguishing predictions of options.
    arch = {"schema_version": "gate-content-key.v1", "family": "options-rehearsal", "rivals": [
        {"explanation": "Use a write-ahead queue so that every request is persisted before it is acknowledged.",
         "distinguishing_prediction": "Choose it when acknowledged requests must survive a process crash."},
        {"explanation": "Use an in-memory ring buffer that drops the oldest entries under load.",
         "distinguishing_prediction": "Choose it when bounded latency matters more than losing old entries."}]}
    good = Builder(tt, binary, base, "options-good")
    opts = good.options(["Use a write-ahead queue so every request is persisted before acknowledgement.",
                         "Use an in-memory ring buffer that drops the oldest entries under load."])
    good.decision(opts, ["Choose it when acknowledged requests must survive a process crash.",
                         "Choose it when bounded latency matters more than losing the oldest entries."])
    straw = Builder(tt, binary, base, "options-straw")
    opts = straw.options(["Use a write-ahead queue so every request is persisted before acknowledgement.",
                          "Do nothing and hope the problem goes away."])
    straw.decision(opts, ["Choose it for good reasons.", "Choose it for good reasons."])
    a = grade_workspace(binary, good.cli.root, arch, "rehearsal")["strawman_rivals"]
    b = grade_workspace(binary, straw.cli.root, arch, "rehearsal")["strawman_rivals"]
    expect("options: good options with distinct reversal conditions are not strawmen", a["strawman"] == 0 and a["denominator"] == 2,
           json.dumps(a["units"])[:800])
    expect("options: an unrelated option and a shared condition are strawmen", b["strawman"] == 2 and b["denominator"] == 2,
           json.dumps(b["units"])[:800])

    # Summary arithmetic and the sample draw.
    graded = [grade_workspace(binary, Builder(tt, binary, base, "sum").cli.root, load_key("tide-gauge-dropout"), name)
              for name in ("model-a", "model-b")]
    summary = summarize(graded)
    expect("summary: groups by agent family with undefined rates for empty runs",
           set(summary["by_agent_family"]) == {"model-a", "model-b"}
           and summary["by_agent_family"]["model-a"]["strawman_rival_rate"]["rate"] is None)
    expect("summary: Wilson interval is sane", wilson(0, 10)[0] == 0.0 and 0.2 < wilson(0, 10)[1] < 0.4 and wilson(3, 0) is None)
    expect("sample: deterministic for one seed", sample(graded, 7, 3) == sample(graded, 7, 3))
    report["checks"], report["failed"] = len(checks), failures
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    g = sub.add_parser("grade")
    g.add_argument("--bin", required=True)
    g.add_argument("--workspace", required=True)
    g.add_argument("--key", required=True, help="a key file, or a development key name in training/gate-content/keys")
    g.add_argument("--agent-family", default="unlabelled")
    g.add_argument("--after", help="JSON file mapping case ID to the setup head sequence; earlier records are not graded")
    g.add_argument("--run-id")
    s = sub.add_parser("summarize")
    s.add_argument("graded", nargs="+")
    m = sub.add_parser("sample")
    m.add_argument("graded", nargs="+")
    m.add_argument("--seed", type=int, required=True)
    m.add_argument("--size", type=int, default=20)
    sub.add_parser("rubric")
    r = sub.add_parser("rehearse")
    r.add_argument("--bin", required=True)
    args = parser.parse_args()
    if getattr(args, "bin", None):
        args.bin = str(Path(args.bin).resolve())
    if args.command == "grade":
        after = json.loads(Path(args.after).read_text(encoding="utf-8")) if args.after else None
        print(json.dumps(grade_workspace(args.bin, args.workspace, load_key(args.key), args.agent_family, after, args.run_id), indent=2))
    elif args.command in ("summarize", "sample"):
        results = [json.loads(Path(path).read_text(encoding="utf-8")) for path in args.graded]
        print(json.dumps(summarize(results) if args.command == "summarize" else sample(results, args.seed, args.size), indent=2))
    elif args.command == "rubric":
        print(json.dumps(rubric(), indent=2))
    else:
        with tempfile.TemporaryDirectory(prefix="gate-content-") as base:
            report = rehearse(args.bin, base)
        print(json.dumps(report, indent=2))
        sys.exit(1 if report["failed"] else 0)


if __name__ == "__main__":
    main()
