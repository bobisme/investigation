//! Advisory measurement flags.
//!
//! Each rule reads typed fields from [`crate::measurement_fields`] and emits a
//! flag with a stable ID, the exact fields it read, and why it fired. Flags are
//! advisory: they never reject a record, never change recorded or effective
//! measurement validity, and never compute a statistic from raw data. The only
//! arithmetic is on recorded counts and a recorded quantile level.
//!
//! Rules fire only from recorded fields. Nothing here assumes a domain: a
//! closed-loop rule fires only when a load model is recorded, a quantile rule
//! only when a quantile statistic is recorded, and so on. An absent field is
//! not evidence of a problem unless a rule says so by name (for example, a
//! quantile without a sample count).
//!
//! Flags describe the exact revisions a record cites. Correcting an
//! Observation clears its own flags; a Comparison citing the earlier revision
//! keeps them until it is revised.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    command::RevisionRef,
    header::Knowledge,
    identity::{Id, Kind},
    input,
    measurement_fields::{
        AttemptCounts, FieldSource, LoadLoop, MeasurementFields, ObservationFields, Statistic,
        StatisticCombination, StatisticKind, StatisticPopulation, TrialOrder, cited_run,
        observation_fields,
    },
    measurement_ops::{VerificationRequest, judgment_log},
    state::{OperationContext, Record, State},
};

/// Version of this rule set. Flag IDs are stable within a version.
pub const FLAG_RULE_VERSION: &str = "measurement-flags.v1";

/// Stated with every non-empty flag report and every receipt warning.
pub const ADVISORY_NOTICE: &str = "Advisory: measurement flags never block recording and never change recorded or effective validity.";

/// Miss probability for the distribution-free order-statistic check.
///
/// With `n` independent samples, every sample falls below the `q` quantile with
/// probability `q^n`. The sample maximum is a one-sided 95% upper confidence
/// bound for that quantile only when `q^n <= 0.05`, whatever the distribution.
/// For `q = 0.99` that needs `n >= 299`; for `q = 0.999`, `n >= 2,995`. This is
/// arithmetic on a recorded count and a recorded level, not a universal sample
/// floor: a smaller sample can still describe what was observed.
pub const ORDER_STATISTIC_MISS_PROBABILITY: f64 = 0.05;

/// Most flags one record's report lists; the rest are counted as omitted.
pub const MAX_FLAGS_PER_RECORD: usize = 32;

/// Most cited Observations compared pairwise for one record (120 pairs).
pub const MAX_PAIRED_OBSERVATIONS: usize = 16;

/// Stable flag IDs for [`FLAG_RULE_VERSION`].
pub mod flag_id {
    pub const QUANTILE_SAMPLE_COUNT_UNRECORDED: &str =
        "measurement.quantile_sample_count_unrecorded";
    pub const QUANTILE_BELOW_ORDER_STATISTIC_BOUND: &str =
        "measurement.quantile_below_order_statistic_bound";
    pub const FLAT_SAMPLES: &str = "measurement.flat_samples";
    pub const SEQUENTIAL_TRIAL_ORDER: &str = "measurement.sequential_trial_order";
    pub const CLOSED_LOOP_WITHOUT_INTENDED_SEND_TIMING: &str =
        "measurement.closed_loop_without_intended_send_timing";
    pub const QUANTILES_COMBINED_ACROSS_RUNS: &str = "measurement.quantiles_combined_across_runs";
    pub const COMPLETION_ONLY_STATISTIC: &str = "measurement.completion_only_statistic";
    pub const INSTRUMENTATION_CHANGED: &str = "measurement.instrumentation_changed";
    pub const COMPARED_WORKLOAD_DIFFERS: &str = "measurement.compared_workload_differs";
    pub const COMPARED_LOAD_MODEL_DIFFERS: &str = "measurement.compared_load_model_differs";
    pub const COMPARED_WARMUP_DIFFERS: &str = "measurement.compared_warmup_differs";
    pub const COMPARED_TOOL_IDENTITY_DIFFERS: &str = "measurement.compared_tool_identity_differs";
    pub const OUTCOME_FROM_COMPLETION_ONLY_STATISTIC: &str =
        "measurement.outcome_from_completion_only_statistic";
}

/// One recorded field a flag read. `recorded` is null when the field is absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FlagField {
    pub record_ref: RevisionRef,
    pub path: String,
    pub recorded: Value,
}

/// One advisory flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MeasurementFlag {
    pub flag_id: String,
    pub rule_version: String,
    /// Always true: the flag never blocks recording or changes validity.
    pub advisory: bool,
    /// The record the flag is about.
    pub subject_ref: RevisionRef,
    /// Other records the rule compared or relied on.
    pub related_refs: Vec<RevisionRef>,
    pub fields: Vec<FlagField>,
    pub reason: String,
}

/// The flags shown with one record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FlagReport {
    pub rule_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    pub flags: Vec<MeasurementFlag>,
    pub omitted: usize,
}

impl FlagReport {
    fn new(mut flags: Vec<MeasurementFlag>) -> Self {
        let mut seen = BTreeSet::new();
        flags.retain(|flag| {
            seen.insert((
                flag.flag_id.clone(),
                flag.subject_ref,
                flag.related_refs.clone(),
            ))
        });
        let omitted = flags.len().saturating_sub(MAX_FLAGS_PER_RECORD);
        flags.truncate(MAX_FLAGS_PER_RECORD);
        Self {
            rule_version: FLAG_RULE_VERSION.into(),
            notice: (!flags.is_empty() || omitted > 0).then(|| ADVISORY_NOTICE.into()),
            flags,
            omitted,
        }
    }
}

/// The smallest sample count whose maximum bounds the `quantile` level from
/// above with 95% confidence. Computed by repeated IEEE multiplication, which
/// is exactly rounded and so platform-independent. Levels are limited to six
/// fractional digits, so this runs at most about three million steps.
pub fn order_statistic_minimum(quantile: f64) -> u64 {
    if !(quantile > 0.0 && quantile < 1.0) {
        return 0;
    }
    let (mut miss, mut count) = (1.0_f64, 0_u64);
    while miss > ORDER_STATISTIC_MISS_PROBABILITY {
        miss *= quantile;
        count += 1;
    }
    count
}

/// An Observation with the Run it cites, and the fields each records.
struct Measured<'a> {
    observation: &'a Record,
    run: Option<&'a Record>,
    fields: ObservationFields,
}

impl<'a> Measured<'a> {
    fn new(state: &'a State, observation: &'a Record) -> Self {
        Self {
            observation,
            run: cited_run(state, observation),
            fields: observation_fields(state, observation),
        }
    }

    fn reference(&self) -> RevisionRef {
        self.observation.reference()
    }

    fn has_fields(&self) -> bool {
        self.fields.observation.is_some() || self.fields.run.is_some()
    }

    /// The first recorded value of a field, with the exact record that holds it.
    fn get<T>(
        &self,
        field: impl Fn(&MeasurementFields) -> Option<&T>,
    ) -> Option<(RevisionRef, &T)> {
        self.fields
            .first(field)
            .map(|(source, value)| (self.source_ref(source), value))
    }

    fn source_ref(&self, source: FieldSource) -> RevisionRef {
        match (source, self.run) {
            (FieldSource::Run, Some(run)) => run.reference(),
            _ => self.reference(),
        }
    }

    /// The read field for a flag, or the Observation's own path when absent.
    fn read<T: Serialize>(
        &self,
        name: &str,
        field: impl Fn(&MeasurementFields) -> Option<&T>,
    ) -> FlagField {
        match self.get(field) {
            Some((record_ref, value)) => field_entry(record_ref, name, value),
            None => FlagField {
                record_ref: self.reference(),
                path: path(name),
                recorded: Value::Null,
            },
        }
    }

    fn statistic(&self) -> Option<(RevisionRef, &Statistic)> {
        match self.get(|fields| fields.statistic.as_ref()) {
            Some((reference, Knowledge::Known(statistic))) => Some((reference, statistic)),
            _ => None,
        }
    }
}

fn path(name: &str) -> String {
    format!("/measurement/{name}")
}

fn field_entry<T: Serialize>(record_ref: RevisionRef, name: &str, value: &T) -> FlagField {
    FlagField {
        record_ref,
        path: path(name),
        recorded: serde_json::to_value(value).unwrap_or(Value::Null),
    }
}

fn flag(
    id: &str,
    subject_ref: RevisionRef,
    related_refs: Vec<RevisionRef>,
    fields: Vec<FlagField>,
    reason: String,
) -> MeasurementFlag {
    MeasurementFlag {
        flag_id: id.into(),
        rule_version: FLAG_RULE_VERSION.into(),
        advisory: true,
        subject_ref,
        related_refs,
        fields,
        reason,
    }
}

fn describe(statistic: &Statistic) -> String {
    match (statistic.kind, &statistic.quantile) {
        (StatisticKind::Quantile, Some(level)) => format!("{} quantile", level.as_str()),
        (StatisticKind::Quantile, None) => "quantile".into(),
        (StatisticKind::Mean, _) => "mean".into(),
        (StatisticKind::Minimum, _) => "minimum".into(),
        (StatisticKind::Maximum, _) => "maximum".into(),
        (StatisticKind::Other, _) => "statistic".into(),
    }
}

fn count_text(count: &Knowledge<u64>) -> String {
    match count {
        Knowledge::Known(count) => count.to_string(),
        Knowledge::Unknown => "unknown".into(),
    }
}

/// Recorded attempt counts by name, such as `started 100, observed 95`.
fn denominators(attempts: Option<&AttemptCounts>) -> String {
    let listed = attempts
        .map(|attempts| {
            attempts
                .entries()
                .map(|(name, count)| format!("{name} {}", count_text(&count.count)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        "no attempt counts are recorded".into()
    } else {
        listed.join(", ")
    }
}

fn known(count: Option<&crate::measurement_fields::AttemptCount>) -> Option<u64> {
    match count.map(|count| &count.count) {
        Some(Knowledge::Known(value)) => Some(*value),
        _ => None,
    }
}

/// Whether the recorded counts show an attempt without an observed outcome.
fn some_not_completed(attempts: &AttemptCounts) -> bool {
    let started_exceeds = matches!(
        (known(attempts.started.as_ref()), known(attempts.observed.as_ref())),
        (Some(started), Some(observed)) if started > observed
    );
    started_exceeds
        || [&attempts.timed_out, &attempts.excluded, &attempts.unknown]
            .into_iter()
            .any(|count| known(count.as_ref()).is_some_and(|count| count > 0))
}

/// Whether the recorded counts show every started attempt was observed.
fn all_completed(attempts: &AttemptCounts) -> bool {
    let equal = matches!(
        (known(attempts.started.as_ref()), known(attempts.observed.as_ref())),
        (Some(started), Some(observed)) if started == observed
    );
    equal
        && [&attempts.timed_out, &attempts.excluded, &attempts.unknown]
            .into_iter()
            .all(|count| count.is_none() || known(count.as_ref()) == Some(0))
}

/// Whether a recorded statistic covers completed attempts only, or may, while
/// the recorded counts show attempts outside it. Returns the read fields and
/// the reason fragment.
fn completion_only(measured: &Measured<'_>) -> Option<(Vec<FlagField>, String)> {
    let (statistic_ref, statistic) = measured.statistic()?;
    let attempts = measured.get(|fields| fields.attempts.as_ref());
    let counts = attempts.map(|(_, attempts)| attempts);
    let fires = match &statistic.population {
        Some(Knowledge::Known(StatisticPopulation::AllAttempts)) => false,
        Some(Knowledge::Known(StatisticPopulation::CompletedAttempts)) => {
            !counts.is_some_and(all_completed)
        }
        _ => counts.is_some_and(some_not_completed),
    };
    if !fires {
        return None;
    }
    let scope = match &statistic.population {
        Some(Knowledge::Known(StatisticPopulation::CompletedAttempts)) => {
            "is computed over completed attempts only"
        }
        _ => {
            "does not record which attempts it covers, while some recorded attempts have no observed outcome"
        }
    };
    let text = format!(
        "The {} {scope}. Recorded attempts: {}.",
        describe(statistic),
        denominators(counts)
    );
    Some((
        vec![
            field_entry(statistic_ref, "statistic", &Knowledge::Known(statistic)),
            measured.read("attempts", |fields| fields.attempts.as_ref()),
        ],
        text,
    ))
}

/// Flags from one Observation's own fields and those of its exact Run.
fn single_flags(measured: &Measured<'_>) -> Vec<MeasurementFlag> {
    let subject = measured.reference();
    let mut flags = Vec::new();
    if let Some((statistic_ref, statistic)) = measured.statistic()
        && let Some(level) = statistic.quantile_level()
    {
        let statistic_field = field_entry(statistic_ref, "statistic", &Knowledge::Known(statistic));
        let count_field = measured.read("sample_count", |fields| fields.sample_count.as_ref());
        let name = describe(statistic);
        match measured.get(|fields| fields.sample_count.as_ref()) {
            Some((_, Knowledge::Known(count))) => {
                let minimum = order_statistic_minimum(level);
                if *count < minimum {
                    flags.push(flag(
                        flag_id::QUANTILE_BELOW_ORDER_STATISTIC_BOUND,
                        subject,
                        vec![],
                        vec![statistic_field, count_field],
                        format!(
                            "The {name} rests on {count} recorded samples. A distribution-free 95% order-statistic upper bound on it needs at least {minimum} samples (the smallest n with level^n <= 0.05), so this count cannot support one. This is arithmetic on the recorded count, not a universal sample floor."
                        ),
                    ));
                }
            }
            _ => flags.push(flag(
                flag_id::QUANTILE_SAMPLE_COUNT_UNRECORDED,
                subject,
                vec![],
                vec![statistic_field, count_field],
                format!(
                    "The {name} has no known sample count, so whether it can support an order-statistic bound cannot be checked."
                ),
            )),
        }
        if let Some(Knowledge::Known(
            StatisticCombination::AveragedRunValues | StatisticCombination::Other,
        )) = &statistic.combination
        {
            flags.push(flag(
                flag_id::QUANTILES_COMBINED_ACROSS_RUNS,
                subject,
                vec![],
                vec![field_entry(statistic_ref, "statistic", &Knowledge::Known(statistic))],
                format!(
                    "The {name} combines quantiles computed separately for each run, by averaging or another method. Quantiles from separate runs are not averaged; combining them needs the raw samples or mergeable histograms."
                ),
            ));
        }
    }
    if let Some((reference, samples)) = measured.get(|fields| fields.samples.as_ref())
        && samples.is_flat()
    {
        let count = match samples {
            crate::measurement_fields::SampleSet::Flat { values, .. } => values.len(),
            crate::measurement_fields::SampleSet::Leveled { samples, .. } => samples.len(),
        };
        flags.push(flag(
            flag_id::FLAT_SAMPLES,
            subject,
            vec![],
            vec![FlagField {
                record_ref: reference,
                path: path("samples"),
                recorded: json!({"shape": "flat", "values": count}),
            }],
            "The raw samples are a flat array without repetition level (build, process, iteration) or acquisition order, so they are unable to support a variance analysis across repetition levels.".into(),
        ));
    }
    if let Some((reference, order @ Knowledge::Known(TrialOrder::Sequential))) =
        measured.get(|fields| fields.trial_order.as_ref())
    {
        flags.push(flag(
            flag_id::SEQUENTIAL_TRIAL_ORDER,
            subject,
            vec![],
            vec![field_entry(reference, "trial_order", order)],
            "The compared trials or conditions ran sequentially (all of one, then the next). A comparison built on sequential runs confounds the condition with anything that drifts over time; interleaved or randomized order separates them.".into(),
        ));
    }
    if let Some((reference, model @ Knowledge::Known(load))) =
        measured.get(|fields| fields.load_model.as_ref())
        && load.load_loop == Knowledge::Known(LoadLoop::Closed)
        && load.timed_from_intended_send != Knowledge::Known(true)
    {
        let timing = match load.timed_from_intended_send {
            Knowledge::Known(_) => "are not measured",
            Knowledge::Unknown => "are not known to be measured",
        };
        flags.push(flag(
            flag_id::CLOSED_LOOP_WITHOUT_INTENDED_SEND_TIMING,
            subject,
            vec![],
            vec![field_entry(reference, "load_model", model)],
            format!(
                "Load came from a closed-loop generator and durations {timing} from the intended send time. New work waited for earlier work, so a stall delays sending and drops out of the measurement (coordinated omission); tail claims from it are flagged."
            ),
        ));
    }
    if let Some((fields, text)) = completion_only(measured) {
        flags.push(flag(
            flag_id::COMPLETION_ONLY_STATISTIC,
            subject,
            vec![],
            fields,
            format!(
                "{text} A criterion over all attempts cannot be satisfied from a completion-only statistic."
            ),
        ));
    }
    if let Some((reference, changed @ Knowledge::Known(true))) =
        measured.get(|fields| fields.instrumentation_changed.as_ref())
    {
        flags.push(flag(
            flag_id::INSTRUMENTATION_CHANGED,
            subject,
            vec![],
            vec![field_entry(reference, "instrumentation_changed", changed)],
            "Instrumentation changed relative to the compared runs, so the change is a possible confounder of any difference.".into(),
        ));
    }
    flags
}

/// Compare one recorded field across two measurements. Fires when both know
/// it and differ, or when only one knows it.
fn differs<T: PartialEq + Serialize>(
    name: &str,
    left: &Measured<'_>,
    right: &Measured<'_>,
    field: impl Fn(&MeasurementFields) -> Option<&Knowledge<T>> + Copy,
) -> Option<(Vec<FlagField>, &'static str)> {
    fn value<'m, T>(
        measured: &'m Measured<'_>,
        field: impl Fn(&MeasurementFields) -> Option<&Knowledge<T>>,
    ) -> Option<&'m T> {
        match measured.get(field) {
            Some((_, Knowledge::Known(value))) => Some(value),
            _ => None,
        }
    }
    let wording = match (value(left, field), value(right, field)) {
        (Some(a), Some(b)) if a != b => "differ in",
        (Some(_), None) | (None, Some(_)) => "do not both record",
        _ => return None,
    };
    Some((
        vec![left.read(name, field), right.read(name, field)],
        wording,
    ))
}

/// Possible confounders between two measurements treated as comparable.
fn pair_flags(
    subject: RevisionRef,
    left: &Measured<'_>,
    right: &Measured<'_>,
) -> Vec<MeasurementFlag> {
    if !left.has_fields() || !right.has_fields() {
        return Vec::new();
    }
    let related = vec![left.reference(), right.reference()];
    let mut flags = Vec::new();
    let mut push = |id: &str, found: Option<(Vec<FlagField>, &str)>, what: &str| {
        if let Some((fields, wording)) = found {
            flags.push(flag(
                id,
                subject,
                related.clone(),
                fields,
                format!(
                    "The compared measurements {wording} {what}. They are not treated as comparable on this field without a recorded reason."
                ),
            ));
        }
    };
    push(
        flag_id::COMPARED_WORKLOAD_DIFFERS,
        differs("workload", left, right, |fields| fields.workload.as_ref()),
        "workload identity; a difference may come from the work measured rather than the condition compared",
    );
    push(
        flag_id::COMPARED_LOAD_MODEL_DIFFERS,
        differs("load_model", left, right, |fields| {
            fields.load_model.as_ref()
        }),
        "load model; statistics taken under different load distributions are not comparable as they stand",
    );
    push(
        flag_id::COMPARED_WARMUP_DIFFERS,
        differs("warmup", left, right, |fields| fields.warmup.as_ref()),
        "warm-up period; settling effects are a possible confounder",
    );
    push(
        flag_id::COMPARED_TOOL_IDENTITY_DIFFERS,
        differs("tool_identity", left, right, |fields| {
            fields.tool_identity.as_ref()
        }),
        "instrument or tool identity; changed instrumentation is a possible confounder",
    );
    flags
}

/// Exact Observation revisions a record cites in its evidence fields, in
/// citation order, from the same case.
fn cited_observations<'a>(state: &'a State, record: &Record) -> Vec<&'a Record> {
    let mut seen = BTreeSet::new();
    let mut cited = Vec::new();
    for field in ["evidence_refs", "support_refs", "contradiction_refs"] {
        for item in record
            .body
            .get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Ok(reference) = input::decode::<RevisionRef>(item) else {
                continue;
            };
            if reference.id.kind() == Kind::Observation
                && seen.insert(reference)
                && let Some(observation) = state
                    .revision(reference)
                    .filter(|cited| cited.header.case_id == record.header.case_id)
            {
                cited.push(observation);
            }
        }
    }
    cited
}

/// Controls a `measurement.assess` judgment compared this Observation with.
fn controls<'a>(state: &'a State, observation: &Record) -> Vec<&'a Record> {
    let Ok(log) = judgment_log(&observation.body) else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    log.judgments
        .iter()
        .flat_map(|judgment| &judgment.verification)
        .filter_map(|verification| match &verification.request {
            VerificationRequest::Control {
                observation_ref, ..
            } => Some(*observation_ref),
            _ => None,
        })
        .filter(|reference| seen.insert(*reference))
        .filter_map(|reference| state.revision(reference))
        .filter(|control| control.header.case_id == observation.header.case_id)
        .collect()
}

fn observation_report(state: &State, observation: &Record) -> Vec<MeasurementFlag> {
    let measured = Measured::new(state, observation);
    let mut flags = single_flags(&measured);
    for control in controls(state, observation) {
        flags.extend(pair_flags(
            measured.reference(),
            &measured,
            &Measured::new(state, control),
        ));
    }
    flags
}

fn citing_report(state: &State, record: &Record) -> Vec<MeasurementFlag> {
    let subject = record.reference();
    let cited: Vec<Measured<'_>> = cited_observations(state, record)
        .into_iter()
        .map(|observation| Measured::new(state, observation))
        .collect();
    let mut flags = Vec::new();
    let outcome = record.body.get("outcome").and_then(Value::as_str);
    if record.header.id.kind() == Kind::Comparison
        && let Some(outcome @ ("meets" | "partially_meets")) = outcome
    {
        for measured in &cited {
            if let Some((fields, text)) = completion_only(measured) {
                flags.push(flag(
                    flag_id::OUTCOME_FROM_COMPLETION_ONLY_STATISTIC,
                    subject,
                    vec![measured.reference()],
                    fields,
                    format!(
                        "This Comparison records `{outcome}` from an Observation whose {text} An all-attempt criterion cannot be satisfied from a completion-only statistic; check the criterion's denominator before relying on this outcome."
                    ),
                ));
            }
        }
    }
    let paired = &cited[..cited.len().min(MAX_PAIRED_OBSERVATIONS)];
    for (index, left) in paired.iter().enumerate() {
        for right in &paired[index + 1..] {
            flags.extend(pair_flags(subject, left, right));
        }
    }
    for measured in &cited {
        flags.extend(single_flags(measured));
    }
    flags
}

/// Advisory flags shown with a record, or `None` for kinds that carry none.
/// Observations report their own fields and their recorded controls; records
/// that cite Observations report the cited Observations' flags, possible
/// confounders between them, and, for a Comparison, an outcome resting on a
/// completion-only statistic.
pub fn record_flags(state: &State, record: &Record) -> Option<FlagReport> {
    let flags = match record.header.id.kind() {
        Kind::Observation => observation_report(state, record),
        Kind::Assessment | Kind::Comparison | Kind::Conclusion | Kind::Decision => {
            citing_report(state, record)
        }
        _ => return None,
    };
    Some(FlagReport::new(flags))
}

fn retracted(body: &Value) -> bool {
    body.get("status").and_then(Value::as_str) == Some("retracted")
        || body.get("retracted").and_then(Value::as_bool) == Some(true)
        || body.get("lifecycle").and_then(Value::as_str) == Some("retracted")
}

/// Every flag whose subject is a current, unretracted record of the case, in
/// acceptance order. Each flag appears once, with the record it is about. This
/// is the case-wide read for `next` and structural audits.
pub fn case_flags(state: &State, case_id: Id) -> Vec<MeasurementFlag> {
    let mut records: Vec<&Record> = state
        .records()
        .filter(|record| record.header.case_id == case_id && !retracted(&record.body))
        .collect();
    records.sort_by_key(|record| (record.header.acceptance_sequence, record.header.id));
    let mut flags = Vec::new();
    for record in records {
        let subject = record.reference();
        let own = match record.header.id.kind() {
            Kind::Observation => observation_report(state, record),
            Kind::Assessment | Kind::Comparison | Kind::Conclusion | Kind::Decision => {
                citing_report(state, record)
            }
            _ => continue,
        };
        flags.extend(own.into_iter().filter(|flag| flag.subject_ref == subject));
    }
    let mut seen = BTreeSet::new();
    flags.retain(|flag| {
        seen.insert((
            flag.flag_id.clone(),
            flag.subject_ref,
            flag.related_refs.clone(),
        ))
    });
    flags
}

/// The receipt text for one flag.
pub fn warning_text(flag: &MeasurementFlag) -> String {
    format!(
        "Advisory measurement flag {} on {}: {} It does not block recording or change validity.",
        flag.flag_id, flag.subject_ref.id, flag.reason
    )
}

/// Add a receipt warning for each flag about the record just written. Never
/// fails and never changes the record.
pub fn warn_recorded(context: &mut OperationContext<'_>, reference: RevisionRef) {
    let warnings: Vec<String> = context
        .exact(reference)
        .and_then(|record| record_flags(context.state(), record))
        .map(|report| {
            report
                .flags
                .iter()
                .filter(|flag| flag.subject_ref == reference)
                .map(warning_text)
                .collect()
        })
        .unwrap_or_default();
    for warning in warnings {
        context.warn(warning);
    }
}

#[cfg(test)]
mod tests;
