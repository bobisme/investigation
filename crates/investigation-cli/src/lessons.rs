//! Retrospective reads (§8.6, §9.1) and lesson follow-through (§16.1).
//!
//! `retrospective` reads the selected case's current and superseded
//! Retrospectives with exact references and the review status of each proposed
//! lesson. `retrospective --lessons` reads, from the selected case, the admitted
//! lessons of other cases. Unadmitted lessons never appear there. Their text is
//! shown only once the reading case has recorded a consultation; `--consult`
//! records it explicitly as an attributed `lesson.consult` write, so a plain
//! read never mutates anything. All lesson text is quoted data.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    classification::{Audience, AudiencePolicy, Classifier, Field},
    command::{Command as RecordCommand, CommandVersion, Operation, RevisionRef},
    header::display_title,
    identity::{Id, Kind},
    lessons::{
        self, AdmissionBody, ConsultationBody, ConsultationEffect, LESSON_RULES_VERSION, LessonKey,
        NO_SIDE_EFFECTS, NOTICE, VISIBILITY_RULE, admissions, consultations, lesson_digest,
        usefulness,
    },
    lessons_ops::CONSULT,
    search_text::TextLabel,
    state::{Record, State},
    wire::{ErrorCode, Failure},
};
use investigation_store::{Store, journal::CaseReplay};
use serde_json::{Value, json};

use crate::{
    Global, Output,
    selection::{select_actor, select_case, workspace_argv},
    shortcuts::{announce_generated_id, generated_command_id},
};

/// Longest highlight text, in characters.
const HIGHLIGHT_CHARS: usize = 160;
/// Most entries of one highlight list.
const HIGHLIGHT_ITEMS: usize = 3;

const USEFULNESS_RULE: &str = "usefulness reports what consulting cases recorded: never_consulted, changed_work (some case recorded that it changed work), never_useful (every consulting case recorded an outcome and none changed work) or outcome_unrecorded. It is not a score and does not rank lessons.";

pub(crate) struct ReadArgs {
    pub(crate) include_restricted: bool,
}

pub(crate) struct LessonArgs {
    pub(crate) include_restricted: bool,
    pub(crate) consult: Vec<String>,
    pub(crate) purpose: Option<String>,
    pub(crate) command_id: Option<String>,
}

fn audience(include_restricted: bool) -> Audience {
    if include_restricted {
        Audience::Restricted
    } else {
        Audience::Internal
    }
}

fn quoted(field: &str, value: &Value) -> Value {
    json!({"field":field,"text":value,"label":TextLabel::AuthoredText,
        "content_trust":TextLabel::AuthoredText.content_trust(),"rendering":"quoted_data"})
}

fn bounded(value: &Value) -> String {
    display_title(value.as_str().unwrap_or_default(), HIGHLIGHT_CHARS)
}

/// One visible consultation of a lesson.
struct Use {
    case_id: Id,
    consultation: RevisionRef,
    effect: Option<ConsultationEffect>,
}

/// Consultations of every lesson, from every case, that the audience may see.
fn usage(
    cases: &BTreeMap<Id, CaseReplay>,
    policy: &AudiencePolicy,
) -> BTreeMap<LessonKey, Vec<Use>> {
    let mut usage: BTreeMap<LessonKey, Vec<Use>> = BTreeMap::new();
    for (case_id, replay) in cases {
        let classifier = Classifier::new(&replay.state);
        for (record, body) in consultations(&replay.state, *case_id) {
            if classifier.may_show(record, Field::Metadata, policy) {
                usage.entry(body.lesson.key()).or_default().push(Use {
                    case_id: *case_id,
                    consultation: record.reference(),
                    effect: body.outcome.map(|outcome| outcome.effect),
                });
            }
        }
    }
    usage
}

/// Supporting and counterexample cases, and recorded usefulness.
fn follow_through(source: Id, uses: &[Use]) -> Value {
    let cases_with = |effect| {
        uses.iter()
            .filter(|entry| entry.effect == Some(effect))
            .map(|entry| entry.case_id)
            .collect::<BTreeSet<_>>()
    };
    let mut supporting = BTreeSet::from([source]);
    supporting.extend(cases_with(ConsultationEffect::ChangedWork));
    let effects: Vec<_> = uses.iter().map(|entry| entry.effect).collect();
    json!({
        "supporting_cases":supporting,
        "counterexample_cases":cases_with(ConsultationEffect::Contradicted),
        "usefulness":usefulness(&effects),
        "consultations":uses.iter().map(|entry| json!({
            "case_id":entry.case_id,"consultation_ref":entry.consultation,
            "effect":entry.effect.map_or("unrecorded", ConsultationEffect::as_str),
        })).collect::<Vec<_>>(),
        "rule":USEFULNESS_RULE,
    })
}

fn admission_view(record: &Record, body: &AdmissionBody) -> Value {
    json!({
        "admission_ref":record.reference(),
        "admitted_by":record.header.created_by,
        "actor_kind_self_reported":body.actor_kind_self_reported,
        "basis":body.basis,
        "basis_kind":body.basis.kind(),
        "attribution":"The actor and its kind are claims, not authentication.",
    })
}

/// The trust of a lesson's text: authored, and resting on evidence that may
/// be untrusted when it cites evidence records.
fn trust(state: &State, lesson: &Value) -> Value {
    let cites_evidence = lesson["support_refs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str()?.parse::<Id>().ok())
        .filter_map(|id| state.current(id))
        .any(|record| {
            matches!(
                record.header.id.kind(),
                Kind::Observation | Kind::Source | Kind::Artifact | Kind::Run
            )
        });
    json!({
        "content_trust":TextLabel::AuthoredText.content_trust(),
        "rendering":"quoted_data",
        "cited_inputs":if cites_evidence { "includes_untrusted_evidence" } else { "authored_records_only" },
        "limits":"Investigation cannot see what else the authoring session read.",
    })
}

/// `retrospective`: the selected case's methodological record.
pub(crate) fn read_case(
    store: &mut Store,
    global: &Global,
    args: &ReadArgs,
) -> Result<Output, Failure> {
    let case = select_case(store, global.case.as_deref())?;
    let cases = store.cases()?;
    let replay = cases
        .get(&case)
        .ok_or_else(|| Failure::new(ErrorCode::UnknownReference, "The case does not exist."))?;
    let policy = AudiencePolicy {
        audience: audience(args.include_restricted),
        placeholders: true,
    };
    let classifier = Classifier::new(&replay.state);
    let usage = usage(&cases, &policy);
    let admitted = admissions(&replay.state, case);
    let mut records: Vec<&Record> = replay
        .state
        .records()
        .filter(|record| {
            record.header.case_id == case && record.header.id.kind() == Kind::Retrospective
        })
        .collect();
    records.sort_by_key(|record| {
        let first = replay
            .state
            .history(record.header.id)
            .next()
            .map_or(0, |first| first.header.acceptance_sequence);
        (first, record.header.id)
    });
    let mut views = Vec::new();
    for record in &records {
        let mut view = json!({
            "id":record.header.id,"revision":record.header.revision.0,
            "status":record.body["status"],"classification":classifier.effective(record).level,
        });
        if !classifier.may_show(record, Field::Content, &policy) {
            view["content_state"] = json!("withheld");
            views.push(view);
            continue;
        }
        let body = &record.body;
        let lessons: Vec<Value> = lessons::lessons(record)
            .iter()
            .enumerate()
            .map(|(index, content)| {
                let key = LessonKey {
                    retrospective_id: record.header.id,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                };
                let admission = admitted.get(&key);
                let mut lesson = json!({
                    "lesson_key":key.to_string(),"index":key.index,
                    "guidance":quoted("guidance", &content["guidance"]),
                    "applies_when":quoted("applies_when", &content["applies_when"]),
                    "transfer_limits":quoted("transfer_limits", &content["transfer_limits"]),
                    "support_refs":content["support_refs"],
                    "lesson_digest":lesson_digest(content),
                    "review_status":if admission.is_some() { "admitted" } else { "proposed_unreviewed" },
                    "visibility":if admission.is_some() { "admitted_for_other_cases" } else { "source_case_only" },
                    "admission":admission.map(|(record, body)| admission_view(record, body)),
                    "trust":trust(&replay.state, content),
                });
                let uses = usage.get(&key).map_or(&[][..], Vec::as_slice);
                if let (Some(fields), Value::Object(more)) =
                    (lesson.as_object_mut(), follow_through(case, uses))
                {
                    fields.extend(more);
                }
                lesson
            })
            .collect();
        let admitted_count = lessons
            .iter()
            .filter(|lesson| lesson["review_status"] == "admitted")
            .count();
        let strings = |field: &str, take: usize, inner: Option<&str>| -> Vec<String> {
            body[field]
                .as_array()
                .into_iter()
                .flatten()
                .take(take)
                .map(|item| bounded(inner.map_or(item, |key| &item[key])))
                .collect()
        };
        let length = |field: &str| body[field].as_array().map_or(0, Vec::len);
        let created = record.header.created_by.to_string();
        let fields = view.as_object_mut().expect("object");
        fields.extend(
            json!({
                "content_state":"shown",
                "created_by":created,
                "recorded_at":record.header.recorded_at,
                "snapshot":body["snapshot"],
                "case_ref":body["case_ref"],
                "successor_ref":body.get("successor_ref").cloned().unwrap_or(Value::Null),
                "superseded_reason":body.get("superseded_reason").cloned().unwrap_or(Value::Null),
                "highlights":{
                    "methods_attempted":strings("methods_attempted", HIGHLIGHT_ITEMS, Some("name")),
                    "residual_unknowns":strings("residual_unknowns", HIGHLIGHT_ITEMS, None),
                    "effective_steps":length("effective_steps"),
                    "invalid_or_misleading_steps":length("invalid_or_misleading_steps"),
                    "lessons":lessons.len(),
                    "admitted_lessons":admitted_count,
                    "label":TextLabel::AuthoredText,"rendering":"quoted_data",
                },
                "evidence_links":body["read_set"],
                "methods_attempted":body["methods_attempted"],
                "effective_steps":body["effective_steps"],
                "invalid_or_misleading_steps":body["invalid_or_misleading_steps"],
                "none_observed_reason":body.get("none_observed_reason").cloned().unwrap_or(Value::Null),
                "costs_or_constraints":body["costs_or_constraints"],
                "residual_unknowns":body["residual_unknowns"],
                "no_reusable_lesson_reason":body.get("no_reusable_lesson_reason").cloned().unwrap_or(Value::Null),
                "lessons":lessons,
                "content_trust":TextLabel::AuthoredText.content_trust(),
                "rendering":"quoted_data",
            })
            .as_object()
            .expect("object")
            .clone(),
        );
        views.push(view);
    }
    let current: Vec<Id> = records
        .iter()
        .filter(|record| record.body["status"] == "recorded")
        .map(|record| record.header.id)
        .collect();
    let mut draft = workspace_argv(store.workspace());
    draft.extend(
        [
            "--case",
            &case.to_string(),
            "retrospective",
            "--draft",
            "--format",
            "json",
        ]
        .map(str::to_owned),
    );
    let value = json!({
        "read_version":"inv.retrospective-read.v1",
        "rules":LESSON_RULES_VERSION,
        "case_id":case,
        "snapshot":replay.snapshot,
        "audience":policy.audience.as_str(),
        "current_retrospectives":current,
        "retrospectives":views,
        "notice":NOTICE,
        "visibility_rule":VISIBILITY_RULE,
        "side_effects":NO_SIDE_EFFECTS,
        "next":[{"kind":"local_read","argv":draft}],
    });
    let text = render_case(&value);
    Ok(Output::success("retrospective_read", value, text))
}

fn render_case(value: &Value) -> String {
    let mut text = format!(
        "case {}  snapshot {}\n",
        value["case_id"].as_str().unwrap_or(""),
        value["snapshot"]["sequence"]
    );
    let retrospectives = value["retrospectives"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if retrospectives.is_empty() {
        text.push_str(
            "No Retrospective is recorded. Run retrospective --draft for the process digest.\n",
        );
    }
    for retrospective in retrospectives {
        text.push_str(&format!(
            "\n{}  revision {}  {}  {}\n",
            retrospective["id"].as_str().unwrap_or(""),
            retrospective["revision"],
            retrospective["status"].as_str().unwrap_or(""),
            retrospective["content_state"].as_str().unwrap_or("shown"),
        ));
        for lesson in retrospective["lessons"].as_array().into_iter().flatten() {
            // Debug formatting quotes and escapes authored text as data.
            text.push_str(&format!(
                "  {}  {}  {}\n    guidance: {:?}\n",
                lesson["lesson_key"].as_str().unwrap_or(""),
                lesson["review_status"].as_str().unwrap_or(""),
                lesson["usefulness"].as_str().unwrap_or(""),
                lesson["guidance"]["text"].as_str().unwrap_or(""),
            ));
        }
    }
    text.push('\n');
    text.push_str(NOTICE);
    text.push('\n');
    text
}

/// An admitted lesson of another case that the audience may see.
struct Visible<'a> {
    source: Id,
    key: LessonKey,
    state: &'a State,
    classifier: Classifier<'a>,
    retrospective: &'a Record,
    content: &'a Value,
    admission: &'a Record,
    body: AdmissionBody,
}

fn visible_lessons<'a>(
    cases: &'a BTreeMap<Id, CaseReplay>,
    reading: Id,
    policy: &AudiencePolicy,
) -> Vec<Visible<'a>> {
    let mut visible = Vec::new();
    for (source, replay) in cases.iter().filter(|(id, _)| **id != reading) {
        let state = &replay.state;
        for (key, (admission, body)) in admissions(state, *source) {
            let classifier = Classifier::new(state);
            let Some(retrospective) = state.revision(body.lesson.retrospective_ref) else {
                continue;
            };
            let Some(content) = lessons::lesson(retrospective, key.index) else {
                continue;
            };
            let shown = [retrospective, admission]
                .into_iter()
                .chain(state.current(*source))
                .all(|record| classifier.may_show(record, Field::Content, policy));
            if !shown || lesson_digest(content) != body.lesson.lesson_digest {
                continue;
            }
            visible.push(Visible {
                source: *source,
                key,
                state,
                classifier,
                retrospective,
                content,
                admission,
                body,
            });
        }
    }
    visible
}

fn pointer(lesson: &Visible<'_>) -> Value {
    json!({
        "source_case_id":lesson.source,
        "retrospective_ref":lesson.body.lesson.retrospective_ref,
        "lesson_index":lesson.key.index,
        "admission_ref":lesson.admission.reference(),
        "lesson_digest":lesson.body.lesson.lesson_digest,
    })
}

fn consult_argv(store: &Store, reading: Id, key: LessonKey) -> Vec<String> {
    let mut argv = workspace_argv(store.workspace());
    argv.extend(
        [
            "--case",
            &reading.to_string(),
            "retrospective",
            "--lessons",
            "--consult",
            &key.to_string(),
            "--format",
            "json",
        ]
        .map(str::to_owned),
    );
    argv
}

/// `retrospective --lessons [--consult KEY]...`: admitted lessons of other cases.
pub(crate) fn read_others(
    store: &mut Store,
    global: &Global,
    args: &LessonArgs,
) -> Result<Output, Failure> {
    let reading = select_case(store, global.case.as_deref())?;
    let policy = AudiencePolicy::strict(audience(args.include_restricted));
    let mut requested = Vec::new();
    for (index, key) in args.consult.iter().enumerate() {
        let key: LessonKey = key
            .parse()
            .map_err(|failure: Failure| failure.at(format!("--consult/{index}")))?;
        if !requested.contains(&key) {
            requested.push(key);
        }
    }
    let mut receipt = Value::Null;
    if !requested.is_empty() {
        if global.read_only {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "Recording a consultation writes to the case. Remove --read-only, or omit --consult to list lessons without text.",
            )
            .at("--read-only"));
        }
        receipt = consult(store, global, reading, &requested, args, &policy)?;
    }
    let cases = store.cases()?;
    let reader = cases
        .get(&reading)
        .ok_or_else(|| Failure::new(ErrorCode::UnknownReference, "The case does not exist."))?;
    let consulted: BTreeMap<LessonKey, (&Record, ConsultationBody)> =
        consultations(&reader.state, reading)
            .into_iter()
            .map(|(record, body)| (body.lesson.key(), (record, body)))
            .collect();
    let usage = usage(&cases, &policy);
    let lessons: Vec<Value> = visible_lessons(&cases, reading, &policy)
        .into_iter()
        .filter(|lesson| requested.is_empty() || requested.contains(&lesson.key))
        .map(|lesson| {
            let consultation = consulted.get(&lesson.key);
            let source_status = lesson
                .state
                .current(lesson.retrospective.header.id)
                .map(|current| json!({
                    "ref":current.reference(),"status":current.body["status"],
                    "successor_ref":current.body.get("successor_ref").cloned().unwrap_or(Value::Null),
                }));
            let mut view = json!({
                "lesson_key":lesson.key.to_string(),
                "source_case_id":lesson.source,
                "source_case_classification":lesson.state.current(lesson.source)
                    .map(|case| lesson.classifier.effective(case).level),
                "classification":lesson.classifier.effective(lesson.retrospective).level,
                "trust":trust(lesson.state, lesson.content),
                "admission":admission_view(lesson.admission, &lesson.body),
                "source_retrospective":source_status,
                "applies_when":quoted("applies_when", &json!(bounded(&lesson.content["applies_when"]))),
                "pointer":pointer(&lesson),
                "consultation":consultation.map(|(record, body)| json!({
                    "consultation_ref":record.reference(),
                    "outcome":body.outcome,
                })),
            });
            let fields = view.as_object_mut().expect("object");
            if consultation.is_some() {
                fields.extend(
                    json!({
                        "content_state":"shown",
                        "guidance":quoted("guidance", &lesson.content["guidance"]),
                        "transfer_limits":quoted("transfer_limits", &lesson.content["transfer_limits"]),
                        "support_refs":lesson.content["support_refs"],
                    })
                    .as_object()
                    .expect("object")
                    .clone(),
                );
            } else {
                fields.insert("content_state".into(), json!("consult_to_read"));
                fields.insert(
                    "consult_argv".into(),
                    json!(consult_argv(store, reading, lesson.key)),
                );
            }
            let uses = usage.get(&lesson.key).map_or(&[][..], Vec::as_slice);
            if let Value::Object(more) = follow_through(lesson.source, uses) {
                fields.extend(more);
            }
            view
        })
        .collect();
    let snapshots: Vec<_> = cases
        .iter()
        .map(|(case_id, replay)| json!({"case_id":case_id,"snapshot":replay.snapshot}))
        .collect();
    let value = json!({
        "read_version":"inv.lesson-read.v1",
        "rules":LESSON_RULES_VERSION,
        "reading_case_id":reading,
        "audience":policy.audience.as_str(),
        "snapshots":snapshots,
        "lessons":lessons,
        "consultation_receipt":receipt,
        "notice":NOTICE,
        "visibility_rule":VISIBILITY_RULE,
        "consultation_rule":"Lesson text is shown to a case only after it records a consultation with --consult or lesson.consult. Record at or before closure whether it changed anything with lesson.record_outcome.",
        "side_effects":NO_SIDE_EFFECTS,
    });
    let text = render_others(&value);
    Ok(Output::success("lesson_read", value, text))
}

/// Record one consultation per requested lesson that the case has not
/// consulted yet, in one attributed transaction.
fn consult(
    store: &mut Store,
    global: &Global,
    reading: Id,
    requested: &[LessonKey],
    args: &LessonArgs,
    policy: &AudiencePolicy,
) -> Result<Value, Failure> {
    let cases = store.cases()?;
    let visible = visible_lessons(&cases, reading, policy);
    let already: BTreeSet<LessonKey> = cases
        .get(&reading)
        .map(|reader| {
            consultations(&reader.state, reading)
                .into_iter()
                .map(|(_, body)| body.lesson.key())
                .collect()
        })
        .unwrap_or_default();
    let mut operations = Vec::new();
    for (index, key) in requested.iter().enumerate() {
        let Some(lesson) = visible.iter().find(|lesson| lesson.key == *key) else {
            let mut list = workspace_argv(store.workspace());
            list.extend(
                [
                    "--case",
                    &reading.to_string(),
                    "retrospective",
                    "--lessons",
                    "--format",
                    "json",
                ]
                .map(str::to_owned),
            );
            return Err(Failure::new(
                ErrorCode::UnknownReference,
                "No admitted lesson of another case visible to this audience has that key.",
            )
            .at(format!("--consult/{index}"))
            .with_argv(list));
        };
        if already.contains(key) {
            continue;
        }
        let mut payload = json!({"lesson":pointer(lesson)});
        if let Some(purpose) = &args.purpose {
            payload["purpose"] = json!(purpose);
        }
        operations.push(Operation {
            op: CONSULT.to_owned(),
            client_ref: None,
            payload,
        });
    }
    if operations.is_empty() {
        return Ok(Value::Null);
    }
    let generated = args.command_id.is_none();
    let command_id = args.command_id.clone().unwrap_or_else(generated_command_id);
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)?;
    let command = RecordCommand {
        schema_version: CommandVersion::V1,
        command_id: command_id.clone(),
        case_id: Some(reading),
        actor,
        actor_descriptor: None,
        preconditions: vec![],
        operations,
    };
    let bytes = serde_json::to_vec(&command).expect("consultation envelope serializes");
    if generated {
        announce_generated_id(&command_id)?;
    }
    let receipt = store.record(&bytes)?;
    Ok(serde_json::to_value(receipt).expect("receipt serializes"))
}

fn render_others(value: &Value) -> String {
    let lessons = value["lessons"].as_array().map_or(&[][..], Vec::as_slice);
    let mut text = format!(
        "{} admitted lesson(s) of other cases visible to case {}\n",
        lessons.len(),
        value["reading_case_id"].as_str().unwrap_or("")
    );
    for lesson in lessons {
        text.push_str(&format!(
            "\n{}  from {}  {}  admitted by {} ({})\n  applies when: {:?}\n",
            lesson["lesson_key"].as_str().unwrap_or(""),
            lesson["source_case_id"].as_str().unwrap_or(""),
            lesson["classification"].as_str().unwrap_or(""),
            lesson["admission"]["admitted_by"].as_str().unwrap_or(""),
            lesson["admission"]["basis_kind"].as_str().unwrap_or(""),
            lesson["applies_when"]["text"].as_str().unwrap_or(""),
        ));
        if lesson["content_state"] == "shown" {
            // Debug formatting quotes and escapes authored text as data.
            text.push_str(&format!(
                "  guidance: {:?}\n  limits: {:?}\n",
                lesson["guidance"]["text"].as_str().unwrap_or(""),
                lesson["transfer_limits"]["text"].as_str().unwrap_or(""),
            ));
        } else {
            text.push_str("  text: record a consultation with --consult to read it\n");
        }
    }
    text.push('\n');
    text.push_str(NOTICE);
    text.push('\n');
    text
}
