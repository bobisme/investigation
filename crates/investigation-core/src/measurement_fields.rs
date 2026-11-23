//! Typed measurement fields shared by Observations and Runs.
//!
//! Every field is optional. An absent field was not recorded; a field whose
//! state is `unknown` was recorded as explicitly unknown. Neither one rejects
//! the record. The core stores what the recorder states and never derives one
//! field from another: a sample count is not counted from the samples, a unit
//! is not copied from the value, and no trial order or load model is assumed.
//! Domain-specific fields such as `load_model` are descriptive only; nothing in
//! the core gives them defaults.
//!
//! Observations keep the block under [`MEASUREMENT_KEY`]; Runs keep it under the
//! same key and may add a [`Repetition`] under [`REPETITION_KEY`]. Readers use
//! [`observation_fields`] and [`run_fields`] to query them.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    command::{Reference, RevisionRef},
    evidence::ObservationValue,
    header::Knowledge,
    input::{self, DecimalString},
    state::{Record, State},
    wire::{ErrorCode, Failure},
};

/// Record body key that holds the [`MeasurementFields`] block.
pub const MEASUREMENT_KEY: &str = "measurement";
/// Run body key that holds the [`Repetition`] identity.
pub const REPETITION_KEY: &str = "repetition";

const TEXT_LIMIT: usize = 2048;
const NAME_LIMIT: usize = 256;
const MAX_COMPONENTS: usize = 64;
const MAX_LIMITATIONS: usize = 32;
const MAX_SAMPLES: usize = 20_000;
const MAX_SETUP_REFS: usize = 32;

/// Named identity components, such as a build, host or version. A component may
/// be explicitly unknown.
pub type IdentityComponents = BTreeMap<String, Knowledge<String>>;

/// Optional typed measurement fields. Absent means not recorded; `unknown`
/// means recorded as explicitly unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasurementFields {
    /// Unit of the measured quantity. On an Observation with a measurement
    /// value, a known unit must equal `value.unit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Knowledge<String>>,
    /// Identity of the workload, input set or procedure that was measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload: Option<Knowledge<String>>,
    /// Length of the window in which samples were taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_window: Option<Knowledge<Quantity>>,
    /// Settling or warm-up period excluded before the observation window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup: Option<Knowledge<Quantity>>,
    /// Number of samples the reported value rests on, as stated by the recorder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<Knowledge<u64>>,
    /// Raw samples, flat or carrying their repetition level and order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<SampleSet>,
    /// Order in which compared trials or conditions were run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_order: Option<Knowledge<TrialOrder>>,
    /// How load was generated, for measurements taken under generated load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_model: Option<Knowledge<LoadModel>>,
    /// Attempt accounting, each count with its definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<AttemptCounts>,
    /// Structured identity of the environment measured in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_identity: Option<Knowledge<IdentityComponents>>,
    /// Structured identity of the instrument or tool that measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_identity: Option<Knowledge<IdentityComponents>>,
    /// Whether instrumentation changed relative to the runs this one is compared with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrumentation_changed: Option<Knowledge<bool>>,
    /// Limits of these measurement conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

/// A decimal quantity with its unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Quantity {
    pub value: DecimalString,
    pub unit: String,
}

/// Raw samples. A flat array is accepted; it carries no repetition level or
/// acquisition order beyond its array position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case", deny_unknown_fields)]
pub enum SampleSet {
    Flat {
        unit: String,
        values: Vec<DecimalString>,
    },
    /// Each sample states its repetition level and acquisition order.
    Leveled { unit: String, samples: Vec<Sample> },
}

/// One raw sample with its repetition level (build, process, iteration).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub value: DecimalString,
    /// Position in acquisition order. Known positions are unique within a set.
    pub order: Knowledge<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<Knowledge<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Knowledge<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration: Option<Knowledge<u64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrialOrder {
    Interleaved,
    Randomized,
    Sequential,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoadLoop {
    /// Work arrives on its own schedule, whether or not earlier work finished.
    Open,
    /// New work waits for earlier work to finish.
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadModel {
    #[serde(rename = "loop")]
    pub load_loop: Knowledge<LoadLoop>,
    /// Whether each duration is measured from the intended send time rather
    /// than the actual send time.
    pub timed_from_intended_send: Knowledge<bool>,
}

/// Started, observed, timed-out, excluded and unknown attempt counts. Each is
/// kept as recorded; the core does not reconcile one count against another.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptCounts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<AttemptCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<AttemptCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<AttemptCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded: Option<AttemptCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<AttemptCount>,
}

/// One attempt count and what it counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptCount {
    pub count: Knowledge<u64>,
    pub definition: Knowledge<String>,
}

/// Identity of one repeat of a measurement procedure. Each repeat is its own
/// Run with a distinct acquisition; repeats share their series and setup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Repetition {
    /// Identity shared by every repeat of the same procedure.
    pub series: String,
    /// Producer namespace and stable ID of this attempt. A second Run with the
    /// same acquisition is rejected.
    pub acquisition: RunAcquisition,
    /// Setup dependencies the repeats share, such as a build Artifact or a
    /// setup Run. Pinned to exact revisions when accepted.
    #[serde(default)]
    pub setup_refs: Vec<Reference>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunAcquisition {
    pub namespace: String,
    pub external_id: String,
}

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn text(value: &str, max: usize, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > max {
        return Err(invalid(
            path,
            format!("Supply nonblank text no longer than {max} characters."),
        ));
    }
    Ok(())
}

fn known_text(value: &Knowledge<String>, max: usize, path: &str) -> Result<(), Failure> {
    match value {
        Knowledge::Unknown => Ok(()),
        Knowledge::Known(value) => text(value, max, &format!("{path}/value")),
    }
}

fn optional_text(value: Option<&Knowledge<String>>, max: usize, path: &str) -> Result<(), Failure> {
    value.map_or(Ok(()), |value| known_text(value, max, path))
}

fn quantity(value: Option<&Knowledge<Quantity>>, path: &str) -> Result<(), Failure> {
    match value {
        Some(Knowledge::Known(quantity)) => {
            text(&quantity.unit, NAME_LIMIT, &format!("{path}/value/unit"))
        }
        _ => Ok(()),
    }
}

fn components(value: Option<&Knowledge<IdentityComponents>>, path: &str) -> Result<(), Failure> {
    let Some(Knowledge::Known(map)) = value else {
        return Ok(());
    };
    if map.is_empty() || map.len() > MAX_COMPONENTS {
        return Err(invalid(
            &format!("{path}/value"),
            "Supply between 1 and 64 named components, or record the identity as unknown.",
        ));
    }
    for (name, component) in map {
        text(name, NAME_LIMIT, &format!("{path}/value"))?;
        known_text(component, NAME_LIMIT, &format!("{path}/value/{name}"))?;
    }
    Ok(())
}

fn samples(set: &SampleSet, path: &str) -> Result<(), Failure> {
    let (unit, len) = match set {
        SampleSet::Flat { unit, values } => (unit, values.len()),
        SampleSet::Leveled { unit, samples } => (unit, samples.len()),
    };
    text(unit, NAME_LIMIT, &format!("{path}/unit"))?;
    if len == 0 || len > MAX_SAMPLES {
        return Err(invalid(
            path,
            format!("Supply between 1 and {MAX_SAMPLES} samples, or omit samples."),
        ));
    }
    if let SampleSet::Leveled { samples, .. } = set {
        let mut seen = BTreeSet::new();
        for (index, sample) in samples.iter().enumerate() {
            let item = format!("{path}/samples/{index}");
            if let Knowledge::Known(order) = sample.order
                && !seen.insert(order)
            {
                return Err(invalid(
                    &format!("{item}/order"),
                    "Each known sample order may appear once.",
                ));
            }
            for (field, value) in [("build", &sample.build), ("process", &sample.process)] {
                optional_text(value.as_ref(), NAME_LIMIT, &format!("{item}/{field}"))?;
            }
        }
    }
    Ok(())
}

impl MeasurementFields {
    /// Check the block's own shape. `path` is the JSON pointer of the block.
    pub fn validate(&self, path: &str) -> Result<(), Failure> {
        optional_text(self.unit.as_ref(), NAME_LIMIT, &format!("{path}/unit"))?;
        optional_text(
            self.workload.as_ref(),
            TEXT_LIMIT,
            &format!("{path}/workload"),
        )?;
        quantity(
            self.observation_window.as_ref(),
            &format!("{path}/observation_window"),
        )?;
        quantity(self.warmup.as_ref(), &format!("{path}/warmup"))?;
        if let Some(set) = &self.samples {
            samples(set, &format!("{path}/samples"))?;
        }
        if let Some(attempts) = &self.attempts {
            for (name, count) in attempts.entries() {
                known_text(
                    &count.definition,
                    TEXT_LIMIT,
                    &format!("{path}/attempts/{name}/definition"),
                )?;
            }
        }
        components(
            self.environment_identity.as_ref(),
            &format!("{path}/environment_identity"),
        )?;
        components(
            self.tool_identity.as_ref(),
            &format!("{path}/tool_identity"),
        )?;
        if self.limitations.len() > MAX_LIMITATIONS {
            return Err(invalid(
                &format!("{path}/limitations"),
                "Supply at most 32 limitations.",
            ));
        }
        for (index, limitation) in self.limitations.iter().enumerate() {
            text(
                limitation,
                TEXT_LIMIT,
                &format!("{path}/limitations/{index}"),
            )?;
        }
        Ok(())
    }

    /// Check the block against the Observation value it describes. A known
    /// unit must agree with a measurement value's unit; it is never copied.
    pub fn validate_for_observation(
        &self,
        value: &ObservationValue,
        path: &str,
    ) -> Result<(), Failure> {
        self.validate(path)?;
        if let (Some(Knowledge::Known(unit)), ObservationValue::Measurement { unit: value, .. }) =
            (&self.unit, value)
            && unit != value
        {
            return Err(invalid(
                &format!("{path}/unit/value"),
                "The measurement unit must equal value.unit when both are recorded.",
            ));
        }
        Ok(())
    }
}

impl AttemptCounts {
    /// Recorded counts by name, in a stable order.
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, &AttemptCount)> {
        [
            ("started", &self.started),
            ("observed", &self.observed),
            ("timed_out", &self.timed_out),
            ("excluded", &self.excluded),
            ("unknown", &self.unknown),
        ]
        .into_iter()
        .filter_map(|(name, count)| count.as_ref().map(|count| (name, count)))
    }
}

impl SampleSet {
    /// Whether the samples carry no repetition level or explicit order.
    pub fn is_flat(&self) -> bool {
        matches!(self, Self::Flat { .. })
    }
}

impl Repetition {
    /// Check the shape. Setup references are pinned by the Run handler.
    pub fn validate(&self, path: &str) -> Result<(), Failure> {
        text(&self.series, NAME_LIMIT, &format!("{path}/series"))?;
        text(
            &self.acquisition.namespace,
            NAME_LIMIT,
            &format!("{path}/acquisition/namespace"),
        )?;
        text(
            &self.acquisition.external_id,
            NAME_LIMIT,
            &format!("{path}/acquisition/external_id"),
        )?;
        if self.setup_refs.len() > MAX_SETUP_REFS {
            return Err(invalid(
                &format!("{path}/setup_refs"),
                "Supply at most 32 setup references.",
            ));
        }
        Ok(())
    }
}

fn fields_in(body: &Value) -> Option<MeasurementFields> {
    match body.get(MEASUREMENT_KEY) {
        None | Some(Value::Null) => None,
        Some(value) => input::decode(value).ok(),
    }
}

/// The measurement fields recorded on an Observation or Run body, if any.
/// Unreadable data reads as not recorded.
pub fn recorded_fields(record: &Record) -> Option<MeasurementFields> {
    fields_in(&record.body)
}

/// The repetition identity recorded on a Run body, if any.
pub fn recorded_repetition(body: &Value) -> Option<Repetition> {
    match body.get(REPETITION_KEY) {
        None | Some(Value::Null) => None,
        Some(value) => input::decode(value).ok(),
    }
}

/// The exact Run revision an Observation cites, in the same case.
pub fn cited_run<'a>(state: &'a State, observation: &Record) -> Option<&'a Record> {
    let run: RevisionRef = input::decode(observation.body.get("run_ref")?).ok()?;
    state
        .revision(run)
        .filter(|record| record.header.case_id == observation.header.case_id)
}

/// The measurement fields of the exact Run an Observation cites.
pub fn run_fields(state: &State, observation: &Record) -> Option<MeasurementFields> {
    cited_run(state, observation).and_then(recorded_fields)
}

/// Fields an Observation records itself, then those of its exact Run. Each
/// source is reported separately; nothing is merged or inferred.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObservationFields {
    pub observation: Option<MeasurementFields>,
    pub run: Option<MeasurementFields>,
}

pub fn observation_fields(state: &State, observation: &Record) -> ObservationFields {
    ObservationFields {
        observation: recorded_fields(observation),
        run: run_fields(state, observation),
    }
}

impl ObservationFields {
    /// The first recorded value of `field`, from the Observation and then its
    /// Run, with which one supplied it.
    pub fn first<T>(
        &self,
        field: impl Fn(&MeasurementFields) -> Option<&T>,
    ) -> Option<(FieldSource, &T)> {
        self.observation
            .as_ref()
            .and_then(&field)
            .map(|value| (FieldSource::Observation, value))
            .or_else(|| {
                self.run
                    .as_ref()
                    .and_then(&field)
                    .map(|value| (FieldSource::Run, value))
            })
    }
}

/// Which record supplied a field.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    Observation,
    Run,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn benchmark_fields() -> Value {
        benchmark()
    }

    fn benchmark() -> Value {
        json!({
            "unit": {"state": "known", "value": "ms"},
            "workload": {"state": "known", "value": "checkout-mix-v3"},
            "observation_window": {"state": "known", "value": {"value": "300", "unit": "s"}},
            "warmup": {"state": "known", "value": {"value": "30", "unit": "s"}},
            "sample_count": {"state": "known", "value": 3},
            "samples": {"shape": "leveled", "unit": "ms", "samples": [
                {"value": "10.1", "order": {"state": "known", "value": 0},
                 "build": {"state": "known", "value": "b41"},
                 "process": {"state": "known", "value": "p1"},
                 "iteration": {"state": "known", "value": 0}},
                {"value": "10.30", "order": {"state": "known", "value": 1},
                 "build": {"state": "known", "value": "b41"},
                 "process": {"state": "unknown"}},
                {"value": "9.9", "order": {"state": "unknown"}}
            ]},
            "trial_order": {"state": "known", "value": "interleaved"},
            "load_model": {"state": "known", "value": {
                "loop": {"state": "known", "value": "open"},
                "timed_from_intended_send": {"state": "known", "value": true}
            }},
            "attempts": {
                "started": {"count": {"state": "known", "value": 100}, "definition": {"state": "known", "value": "Requests sent."}},
                "observed": {"count": {"state": "known", "value": 95}, "definition": {"state": "known", "value": "Responses received."}},
                "timed_out": {"count": {"state": "known", "value": 5}, "definition": {"state": "known", "value": "No response within 1 s."}},
                "excluded": {"count": {"state": "known", "value": 0}, "definition": {"state": "unknown"}},
                "unknown": {"count": {"state": "unknown"}, "definition": {"state": "unknown"}}
            },
            "environment_identity": {"state": "known", "value": {
                "build": {"state": "known", "value": "b41"},
                "host": {"state": "unknown"}
            }},
            "tool_identity": {"state": "known", "value": {"name": {"state": "known", "value": "generator"}}},
            "instrumentation_changed": {"state": "known", "value": false},
            "limitations": ["One host only."]
        })
    }

    #[test]
    fn every_field_round_trips_exactly() {
        let value = benchmark();
        let fields: MeasurementFields = input::decode(&value).unwrap();
        fields.validate("/measurement").unwrap();
        assert_eq!(serde_json::to_value(&fields).unwrap(), value);
    }

    #[test]
    fn every_field_may_be_explicitly_unknown() {
        let unknown = json!({"state": "unknown"});
        let value = json!({
            "unit": unknown, "workload": unknown, "observation_window": unknown,
            "warmup": unknown, "sample_count": unknown, "trial_order": unknown,
            "load_model": unknown, "environment_identity": unknown,
            "tool_identity": unknown, "instrumentation_changed": unknown,
            "attempts": {"started": {"count": unknown, "definition": unknown}}
        });
        let fields: MeasurementFields = input::decode(&value).unwrap();
        fields.validate("/measurement").unwrap();
        assert_eq!(serde_json::to_value(&fields).unwrap(), value);
        assert_eq!(fields.sample_count, Some(Knowledge::Unknown));
    }

    #[test]
    fn a_flat_sample_array_is_accepted_and_reported_flat() {
        let value =
            json!({"samples": {"shape": "flat", "unit": "count", "values": ["3", "4.50", "-1"]}});
        let fields: MeasurementFields = input::decode(&value).unwrap();
        fields.validate("/measurement").unwrap();
        assert!(fields.samples.as_ref().unwrap().is_flat());
        assert_eq!(serde_json::to_value(&fields).unwrap(), value);
    }

    #[test]
    fn nothing_is_inferred_from_another_field() {
        let value = json!({"samples": {"shape": "flat", "unit": "ms", "values": ["1", "2"]}});
        let fields: MeasurementFields = input::decode(&value).unwrap();
        assert_eq!(fields.sample_count, None);
        assert_eq!(fields.unit, None);
        assert_eq!(fields.trial_order, None);
        assert_eq!(fields.load_model, None);
        assert_eq!(serde_json::to_value(&fields).unwrap(), value);
    }

    #[test]
    fn malformed_fields_are_rejected_with_their_path() {
        let cases = [
            (
                json!({"samples": {"shape": "flat", "unit": "ms", "values": []}}),
                "/measurement/samples",
            ),
            (
                json!({"samples": {"shape": "leveled", "unit": "ms", "samples": [
                    {"value": "1", "order": {"state": "known", "value": 2}},
                    {"value": "2", "order": {"state": "known", "value": 2}}
                ]}}),
                "/measurement/samples/samples/1/order",
            ),
            (
                json!({"environment_identity": {"state": "known", "value": {}}}),
                "/measurement/environment_identity/value",
            ),
            (
                json!({"workload": {"state": "known", "value": " "}}),
                "/measurement/workload/value",
            ),
            (
                json!({"attempts": {"started": {"count": {"state": "known", "value": 1}, "definition": {"state": "known", "value": ""}}}}),
                "/measurement/attempts/started/definition/value",
            ),
        ];
        for (value, path) in cases {
            let fields: MeasurementFields = input::decode(&value).unwrap();
            let failure = fields.validate("/measurement").unwrap_err();
            assert_eq!(failure.error.path.as_deref(), Some(path), "{value}");
        }
        assert!(input::decode::<MeasurementFields>(&json!({"samples": ["1", "2"]})).is_err());
        assert!(
            input::decode::<MeasurementFields>(
                &json!({"trial_order": {"state": "known", "value": "a_then_b"}})
            )
            .is_err()
        );
        assert!(
            input::decode::<MeasurementFields>(
                &json!({"sample_count": {"state": "known", "value": "12"}})
            )
            .is_err()
        );
        assert!(
            input::decode::<MeasurementFields>(
                &json!({"samples": {"shape": "flat", "unit": "ms", "values": [1.5]}})
            )
            .is_err()
        );
    }

    #[test]
    fn a_known_unit_must_match_a_measurement_value() {
        let fields: MeasurementFields =
            input::decode(&json!({"unit": {"state": "known", "value": "s"}})).unwrap();
        let value: ObservationValue =
            input::decode(&json!({"kind": "measurement", "value": "1", "unit": "ms"})).unwrap();
        let failure = fields
            .validate_for_observation(&value, "/measurement")
            .unwrap_err();
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/measurement/unit/value")
        );
        let text: ObservationValue =
            input::decode(&json!({"kind": "text", "statement": "seen"})).unwrap();
        fields
            .validate_for_observation(&text, "/measurement")
            .unwrap();
    }
}

#[cfg(test)]
mod operation_tests {
    use super::*;
    use crate::{
        catalog::standard_catalog,
        command::Command,
        header::SystemClock,
        identity::{Id, SystemIdSource},
        measurement_ops::{Qualification, judgment_log},
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
    };
    use serde_json::json;

    struct Case {
        state: State,
        case: Option<Id>,
    }

    impl Case {
        fn new() -> Self {
            let mut test = Self {
                state: State::default(),
                case: None,
            };
            let accepted = test
                .submit(
                    "case.create",
                    json!({"title": "Measurement fields", "mode": "record"}),
                )
                .unwrap();
            test.case = Some(accepted.case_id);
            test
        }

        fn submit(&mut self, op: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version": "inv.command.v1",
                "command_id": format!("command-{sequence}"),
                "case_id": self.case,
                "actor": "agent:test",
                "preconditions": [],
                "operations": [{"op": op, "payload": payload}],
            }))
            .unwrap();
            let accepted = validate_and_apply(
                &self.state,
                &command,
                &standard_catalog(),
                Acceptance {
                    sequence,
                    clock: &mut SystemClock,
                    ids: &mut SystemIdSource,
                },
            )?;
            self.state.apply(&accepted)?;
            Ok(accepted)
        }

        fn create(&mut self, op: &str, payload: Value) -> RevisionRef {
            self.submit(op, payload)
                .unwrap()
                .records
                .last()
                .unwrap()
                .reference()
        }

        fn body(&self, reference: RevisionRef) -> &Value {
            &self.state.current(reference.id).unwrap().body
        }
    }

    fn run(measurement: Option<Value>, repetition: Option<Value>) -> Value {
        let mut payload = json!({
            "intent": {"kind": "exploration", "intent": "Measure the procedure."},
            "producer": "agent:producer",
            "artifact_refs": [],
            "execution_status": "completed",
            "evidence_completeness": "complete",
            "evidence_kind": "observational",
            "variables_changed": [],
        });
        if let Some(measurement) = measurement {
            payload["measurement"] = measurement;
        }
        if let Some(repetition) = repetition {
            payload["repetition"] = repetition;
        }
        payload
    }

    fn observation(measurement: Option<Value>, run: Option<RevisionRef>) -> Value {
        let mut payload = json!({
            "value": {"kind": "measurement", "value": "12.50", "unit": "ms"},
            "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
            "extraction_method": null,
            "validity": "valid",
            "quality": [],
            "limitations": [],
            "evidence_kind": "observational",
            "variables_changed": [],
            "observed_at": null,
            "run_ref": run,
        });
        if let Some(measurement) = measurement {
            payload["measurement"] = measurement;
        }
        payload
    }

    fn environment(build: &str) -> Value {
        json!({"environment_identity": {"state": "known", "value": {
            "build": {"state": "known", "value": build},
            "host": {"state": "unknown"}
        }}})
    }

    #[test]
    fn observation_and_run_fields_round_trip_through_their_operations() {
        let mut case = Case::new();
        let fields = super::tests::benchmark_fields();
        let run_ref = case.create("run.record", run(Some(fields.clone()), None));
        assert_eq!(case.body(run_ref)[MEASUREMENT_KEY], fields);
        let observed = case.create(
            "observation.add",
            observation(Some(fields.clone()), Some(run_ref)),
        );
        assert_eq!(case.body(observed)[MEASUREMENT_KEY], fields);
        let record = case.state.current(observed.id).unwrap();
        let both = observation_fields(&case.state, record);
        assert_eq!(both.observation, both.run);
        assert_eq!(
            both.first(|fields| fields.sample_count.as_ref()),
            Some((FieldSource::Observation, &Knowledge::Known(3)))
        );

        let declared = case.create(
            "run.start",
            json!({
                "intent": {"kind": "exploration", "intent": "Measure later."},
                "producer": "agent:producer", "tool": null, "environment": null,
                "external_ref": null, "evidence_kind": "observational",
                "measurement": {"workload": {"state": "unknown"}}
            }),
        );
        assert_eq!(
            case.body(declared)[MEASUREMENT_KEY],
            json!({"workload": {"state": "unknown"}})
        );

        // Records without the block keep their exact prior shape.
        let plain = case.create("observation.add", observation(None, None));
        assert!(case.body(plain).get(MEASUREMENT_KEY).is_none());
        let plain_run = case.create("run.record", run(None, None));
        assert!(case.body(plain_run).get(MEASUREMENT_KEY).is_none());
        assert!(case.body(plain_run).get(REPETITION_KEY).is_none());
    }

    #[test]
    fn unknown_fields_and_flat_samples_never_reject_the_observation() {
        let mut case = Case::new();
        let unknown = json!({"state": "unknown"});
        let all_unknown = json!({
            "unit": unknown, "workload": unknown, "observation_window": unknown,
            "warmup": unknown, "sample_count": unknown, "trial_order": unknown,
            "load_model": unknown, "environment_identity": unknown,
            "tool_identity": unknown, "instrumentation_changed": unknown,
            "attempts": {"timed_out": {"count": unknown, "definition": unknown}},
            "samples": {"shape": "flat", "unit": "ms", "values": ["10", "11.5"]}
        });
        let accepted = case.create(
            "observation.add",
            observation(Some(all_unknown.clone()), None),
        );
        assert_eq!(case.body(accepted)[MEASUREMENT_KEY], all_unknown);

        let failure = case
            .submit(
                "observation.add",
                observation(
                    Some(json!({"unit": {"state": "known", "value": "s"}})),
                    None,
                ),
            )
            .unwrap_err();
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/operations/0/payload/measurement/unit/value")
        );
    }

    #[test]
    fn a_repeat_is_a_distinct_run_with_its_own_acquisition_and_shared_setup() {
        let mut case = Case::new();
        let setup = case.create("run.record", run(None, None));
        let repeat = |id: &str| {
            json!({
                "series": "procedure-7",
                "acquisition": {"namespace": "producer", "external_id": id},
                "setup_refs": [setup]
            })
        };
        let first = case.create("run.record", run(None, Some(repeat("attempt-1"))));
        let second = case.create("run.record", run(None, Some(repeat("attempt-2"))));
        assert_ne!(first.id, second.id);
        for reference in [first, second] {
            let repetition = recorded_repetition(case.body(reference)).unwrap();
            assert_eq!(repetition.series, "procedure-7");
            assert_eq!(repetition.setup_refs, vec![Reference::Existing(setup)]);
        }
        assert_ne!(
            recorded_repetition(case.body(first)).unwrap().acquisition,
            recorded_repetition(case.body(second)).unwrap().acquisition
        );
        // Shared setup is a tracked dependency of each repeat.
        let edges =
            crate::freshness::dependency_edges(case.state.current(first.id).unwrap()).unwrap();
        assert!(
            edges
                .iter()
                .any(|edge| edge.field == "repetition.setup_refs" && edge.target == setup)
        );

        let duplicate = case
            .submit("run.record", run(None, Some(repeat("attempt-1"))))
            .unwrap_err();
        assert_eq!(duplicate.error.code, ErrorCode::GateFailed);
        assert_eq!(
            duplicate.error.path.as_deref(),
            Some("/operations/0/payload/repetition/acquisition")
        );
        assert_eq!(duplicate.error.actual, Some(json!(first)));

        let missing = case
            .submit(
                "run.record",
                run(
                    None,
                    Some(json!({
                        "series": "procedure-7",
                        "acquisition": {"namespace": "producer", "external_id": "attempt-3"},
                        "setup_refs": [{"id": "run_019acd16-0000-7000-8000-00000000ffff", "revision": 1}]
                    })),
                ),
            )
            .unwrap_err();
        assert_eq!(missing.error.code, ErrorCode::UnknownReference);

        // Reconciling a declared repeat must keep its identity.
        let declared = case.create(
            "run.start",
            json!({
                "intent": {"kind": "exploration", "intent": "Measure the procedure."},
                "producer": "agent:producer", "tool": null, "environment": null,
                "external_ref": null, "evidence_kind": "observational",
                "repetition": repeat("attempt-4")
            }),
        );
        let mut outcome = run(None, Some(repeat("attempt-5")));
        let reconcile = |outcome: &Value| {
            json!({"run_ref": declared, "outcome": outcome, "evidence_validity": null,
                   "external_ref": null, "retrospective_plan_ref": null,
                   "rationale": "The producer finished."})
        };
        for key in [
            "tool",
            "environment",
            "started_at",
            "finished_at",
            "result_assertion",
            "manifest_digest",
            "search_coverage",
        ] {
            outcome[key] = Value::Null;
        }
        let changed = case
            .submit("run.reconcile", reconcile(&outcome))
            .unwrap_err();
        assert_eq!(
            changed.error.path.as_deref(),
            Some("/operations/0/payload/outcome/repetition")
        );
        outcome["repetition"] = repeat("attempt-4");
        let reconciled = case.create("run.reconcile", reconcile(&outcome));
        assert_eq!(reconciled.id, declared.id);
        assert_eq!(
            recorded_repetition(case.body(reconciled))
                .unwrap()
                .acquisition
                .external_id,
            "attempt-4"
        );
    }

    #[test]
    fn structured_identity_extends_control_comparability() {
        let mut case = Case::new();
        let target_run = case.create("run.record", run(Some(environment("b41")), None));
        let target = case.create("observation.add", observation(None, Some(target_run)));
        let wrong_run = case.create("run.record", run(Some(environment("b40")), None));
        let wrong = case.create("observation.add", observation(None, Some(wrong_run)));
        // The control's own fields take precedence over its Run's.
        let right = case.create(
            "observation.add",
            observation(Some(environment("b41")), Some(wrong_run)),
        );
        let bare = case.create("observation.add", observation(None, None));
        let mut tooled = environment("b41");
        tooled["tool_identity"] =
            json!({"state": "known", "value": {"version": {"state": "known", "value": "2"}}});
        let tooled = case.create("observation.add", observation(Some(tooled), None));

        let assess =
            |case: &mut Case, target: RevisionRef, control: RevisionRef, validity: &str| {
                case.submit(
                    "measurement.assess",
                    json!({
                        "observation_ref": target, "channel": null, "validity": validity,
                        "verification": [{"kind": "control", "observation_ref": control,
                            "expected_environment": null, "rationale": "Same setup."}],
                        "rationale": "Judged from the recorded setup.", "limits": []
                    }),
                )
            };
        for (control, reason) in [
            (wrong, "CONTROL_ENVIRONMENT_COMPONENT_MISMATCH"),
            (bare, "CONTROL_ENVIRONMENT_IDENTITY_UNRECORDED"),
        ] {
            let failure = assess(&mut case, target, control, "valid").unwrap_err();
            assert_eq!(failure.error.code, ErrorCode::GateFailed);
            assert!(
                failure.error.message.contains(reason),
                "{}",
                failure.error.message
            );
            assert!(
                !failure
                    .error
                    .message
                    .contains("REQUIRED_ENVIRONMENT_UNRECORDED")
            );
        }

        let accepted = assess(&mut case, target, right, "valid").unwrap();
        let target = accepted.records[0].reference();
        let log = judgment_log(case.body(target)).unwrap();
        let verification = &log.judgments[0].verification[0];
        assert_eq!(verification.qualification, Qualification::Qualified);
        let compared = verification.compared_environments.as_ref().unwrap();
        assert_eq!(compared.required, None);
        assert_eq!(compared.components.len(), 1);
        assert_eq!(compared.components[0].component, "build");
        assert_eq!(compared.components[0].control.as_deref(), Some("b41"));

        // A target that records a tool identity needs the control's to match.
        let failure = assess(&mut case, tooled, right, "valid").unwrap_err();
        assert!(
            failure
                .error
                .message
                .contains("CONTROL_TOOL_IDENTITY_UNRECORDED")
        );
    }
}
