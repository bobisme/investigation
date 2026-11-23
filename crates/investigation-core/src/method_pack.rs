//! Declarative method packs (`inv.method-pack.v1`) and their inert loader.
//!
//! A method pack is read-only data: guidance cards, contextual triggers from a
//! closed vocabulary, optional namespaced property schemas, observation
//! guidance, promotion-policy proposals and references to views and report
//! templates. It cannot execute commands, load code, contact services or grant
//! authority, and this module never interprets any pack value as an
//! instruction. [`load`] is a pure function of the supplied bytes: it performs
//! no process spawn, network access or write, and the same bytes always yield
//! the same digest and parsed value. [`load_file`] adds a single bounded read.
//!
//! Files are TOML so that authors can write multi-line guidance. The decoder
//! applies the strict input rules used for versioned JSON requests: bounded
//! size and nesting, no duplicate keys, no non-finite or unsafe numbers, no
//! dates, and every unknown key rejected with a JSON Pointer path. Keys that
//! look like hooks, commands, includes or remote references are rejected before
//! the typed decode, so such a pack is refused rather than partly honored.
//!
//! Card text becomes guidance only for the built-in pack or a pack pinned to a
//! case; loading a pack does not adopt it.

use std::{collections::BTreeSet, fmt, io::Read, path::Path};

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::{
    case::CaseMode,
    catalog::{Status, standard_catalog},
    identity::Kind,
    input::{self, InputError, pointer},
    wire::{Digest, ErrorCode, Failure},
};

pub const SCHEMA_VERSION: &str = "inv.method-pack.v1";
pub const MAX_PACK_BYTES: usize = 256 * 1024;
pub const MAX_PACK_DEPTH: usize = 16;
pub const MAX_NAME_BYTES: usize = 64;
pub const MAX_LINE_BYTES: usize = 200;
pub const MAX_TEXT_BYTES: usize = 2048;
pub const MAX_CARD_BODY_BYTES: usize = 4096;
pub const MAX_ENTRIES: usize = 128;
/// Upper bound for `max_length` in a declared string property.
pub const MAX_PROPERTY_STRING_BYTES: u32 = 4096;
/// Upper bound for `max_items` in a declared array property.
pub const MAX_PROPERTY_ITEMS: u32 = 256;

/// The source of the built-in `general` pack, compiled into the binary.
pub const GENERAL_PACK_SOURCE: &[u8] = include_bytes!("../../../methods/general.toml");

/// Gap-rule IDs a card trigger may reference. Packs cannot define predicates;
/// they can only point at rules the core already evaluates deterministically.
pub const GAP_RULE_TRIGGERS: &[&str] = &[
    "artifact.repeated_identical_bytes",
    "case.blockers_unchanged",
    "case.collection_without_consequence",
    "case.planned_inquiries_exhausted",
    "case.preserve_declared_time_sensitive_source",
    "case.ready_to_close_without_retrospective",
    "comparison.stale_dependency_revision",
    "conclusion.causal_claim_without_rival_account",
    "conclusion.disputed",
    "decision.accepted_with_unresolved_hard_cell",
    "decision.hard_criterion_cell_missing",
    "decision.hard_criterion_cell_unknown",
    "decision.option_criterion_cell_missing",
    "freshness.needs_review",
    "hypothesis.no_assessment",
    "inquiry.confirmatory_plan_unfrozen",
    "observation.invalid_or_missing_source_match",
    "observation.validity_context_missing",
    "review.stale_read_set",
    "run.outcome_incomplete_or_unknown",
    "run.repeated_same_failure",
];

/// Operations a policy proposal may attach requirements to. Policies only add
/// requirements to promotion and closure; no policy can gate recording.
pub const POLICY_GATES: &[&str] = &[
    "case.close",
    "conclusion.request_review",
    "conclusion.validate",
    "decision.accept",
    "decision.propose",
];

/// A validated pack plus the SHA-256 digest of the exact bytes it came from.
/// It exposes no mutation; the only way to obtain one is through [`load`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPack {
    pack: MethodPack,
    digest: Digest,
    byte_len: usize,
}

impl LoadedPack {
    pub fn pack(&self) -> &MethodPack {
        &self.pack
    }

    pub fn id(&self) -> &str {
        &self.pack.id
    }

    pub fn version(&self) -> &str {
        &self.pack.version
    }

    /// `sha256:` digest over the file bytes, not over a normalized form.
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    pub fn card(&self, name: &str) -> Option<&Card> {
        self.pack.cards.iter().find(|card| card.name == name)
    }
}

/// `inv.method-pack.v1`. Every list defaults to empty; unknown keys are errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MethodPack {
    /// Exactly `inv.method-pack.v1`.
    pub schema_version: String,
    /// Lowercase kebab-case identifier; also the extension namespace.
    pub id: String,
    /// `MAJOR.MINOR.PATCH`.
    pub version: String,
    pub title: String,
    pub summary: String,
    /// Case modes the pack applies to.
    pub modes: Vec<CaseMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cards: Vec<Card>,
    /// Optional properties, recorded under `extensions.<pack id>.<name>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_guidance: Vec<ObservationGuidance>,
    /// Proposed promotion requirements. They take effect only for a case that
    /// pins this pack, and only at the declared gate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<PolicyProposal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<ViewReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<ReportReference>,
}

/// One short guidance card. Its text is plain prose, never a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub name: String,
    pub title: String,
    /// A workflow stage from the operation catalog.
    pub stage: String,
    /// One line describing when the card applies.
    pub summary: String,
    /// Restricts the card to some of the pack's modes; empty means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<CaseMode>,
    /// Any one of these references makes the card relevant. Empty means the
    /// card is only shown on request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
    /// Active catalog operations the card tells the reader to use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
    pub body: CardBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CardBody {
    /// When the card applies.
    pub when: String,
    /// What to record.
    pub record: String,
    /// A common mistake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mistake: Option<String>,
    /// When it is reasonable to skip the card.
    pub skip: String,
}

/// A reference into the closed trigger vocabulary. Exactly one field is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Trigger {
    /// A built-in gap-rule ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_fact: Option<CaseFact>,
    /// The case holds at least one record of this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_kind: Option<RecordKind>,
}

/// Structural case facts derived from recorded fields only.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CaseFact {
    /// The case has no Question.
    NoQuestion,
    /// Exactly one Hypothesis is open.
    SingleOpenHypothesis,
    /// Two or more Hypotheses are open.
    CompetingHypotheses,
    /// Two or more Options are open.
    MultipleOptions,
    /// The case cites two or more Sources.
    MultipleSources,
    /// At least one Observation carries a measured value.
    MeasuredObservation,
    /// The stalled-investigation detector reports repeated inconclusive work.
    Stalled,
}

/// Case-scoped record kinds a pack may name.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Case,
    Question,
    Source,
    Artifact,
    Observation,
    Hypothesis,
    Assumption,
    Option,
    Criterion,
    Comparison,
    Assessment,
    Conclusion,
    Decision,
    Review,
    Objection,
    Annotation,
    Retrospective,
    Inquiry,
    Run,
}

impl RecordKind {
    pub const fn kind(self) -> Kind {
        match self {
            Self::Case => Kind::Case,
            Self::Question => Kind::Question,
            Self::Source => Kind::Source,
            Self::Artifact => Kind::Artifact,
            Self::Observation => Kind::Observation,
            Self::Hypothesis => Kind::Hypothesis,
            Self::Assumption => Kind::Assumption,
            Self::Option => Kind::Option,
            Self::Criterion => Kind::Criterion,
            Self::Comparison => Kind::Comparison,
            Self::Assessment => Kind::Assessment,
            Self::Conclusion => Kind::Conclusion,
            Self::Decision => Kind::Decision,
            Self::Review => Kind::Review,
            Self::Objection => Kind::Objection,
            Self::Annotation => Kind::Annotation,
            Self::Retrospective => Kind::Retrospective,
            Self::Inquiry => Kind::Inquiry,
            Self::Run => Kind::Run,
        }
    }
}

/// An optional field a pack proposes for some record kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyDefinition {
    /// Snake-case field name inside the pack's extension namespace.
    pub name: String,
    pub record_kinds: Vec<RecordKind>,
    pub description: String,
    pub schema: ValueSchema,
}

/// A closed schema subset: bounded scalars, enumerations, arrays and objects.
/// There are no references, patterns or conditional keywords.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValueSchema {
    #[serde(rename = "type")]
    pub value_type: ValueType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `string` only: the permitted values.
    #[serde(default, rename = "enum", skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<String>>,
    /// Required for `string`: maximum UTF-8 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    /// `integer` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    /// `integer` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<i64>,
    /// Required for `array`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<ValueSchema>>,
    /// Required for `array`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u32>,
    /// Required for `object`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldSchema>>,
    /// `object` only: names of required fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    String,
    Integer,
    /// A base-10 decimal string such as `18.70`.
    Decimal,
    Boolean,
    Array,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldSchema {
    pub name: String,
    pub schema: ValueSchema,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationGuidance {
    pub name: String,
    pub title: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<CaseMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicyProposal {
    pub name: String,
    pub input: PolicyInput,
    /// One of the promotion or closure operations in [`POLICY_GATES`].
    pub gate: String,
    /// `required_fields` only: dotted field names that must be present and
    /// nonempty on the gated record (the Conclusion, Decision or Case).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// `closure_retrospective` only: case facts or record kinds that make the
    /// case material for closure. Empty means every closing case is material.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
    pub rationale: String,
}

/// The enumerated policy inputs. Each can only add a requirement at its gate;
/// none can relax a built-in gate or apply to recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyInput {
    /// Named fields must be present and nonempty on the gated record.
    RequiredFields,
    /// A current blindspot review of the gated Conclusion or Decision.
    BlindspotReview,
    /// A Retrospective, not a waiver, when the declared triggers hold.
    ClosureRetrospective,
    /// The promoting actor did not author the subject's current content.
    DistinctActor,
    /// Every open objection, not only critical ones, needs an acknowledgment.
    AcknowledgeOpenObjections,
}

impl PolicyInput {
    /// The gates this input may be declared at.
    pub const fn gates(self) -> &'static [&'static str] {
        match self {
            Self::RequiredFields => POLICY_GATES,
            Self::BlindspotReview | Self::DistinctActor => {
                &["conclusion.validate", "decision.accept"]
            }
            Self::ClosureRetrospective => &["case.close"],
            Self::AcknowledgeOpenObjections => &["decision.accept"],
        }
    }
}

/// A named view built from a renderer-provided view kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewReference {
    pub name: String,
    pub title: String,
    /// Identifier of a renderer-provided view; never a path or URL.
    pub view: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub record_kinds: Vec<RecordKind>,
}

/// A named report built from a renderer-provided template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportReference {
    pub name: String,
    pub title: String,
    /// Identifier of a renderer-provided template; never a path, URL or code.
    pub template: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackErrorKind {
    Unreadable,
    TooLarge,
    TooDeep,
    InvalidUtf8,
    InvalidToml,
    UnsupportedValue,
    UnsafeNumber,
    ExecutableField,
    UnsupportedVersion,
    InvalidField,
    TextTooLong,
    InvalidText,
    InvalidName,
    UnknownReference,
    Duplicate,
    InvalidSchema,
}

/// A bounded, input-free rejection with the JSON Pointer of the failing field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackError {
    pub kind: PackErrorKind,
    pub path: String,
    pub message: &'static str,
    /// 1-based line for TOML syntax errors.
    pub line: Option<usize>,
}

impl PackError {
    fn new(kind: PackErrorKind, path: impl Into<String>, message: &'static str) -> Self {
        Self {
            kind,
            path: path.into(),
            message,
            line: None,
        }
    }
}

impl fmt::Display for PackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(formatter, "line {line}: {}", self.message),
            None => write!(formatter, "{}: {}", self.path, self.message),
        }
    }
}

impl std::error::Error for PackError {}

impl From<PackError> for Failure {
    fn from(error: PackError) -> Self {
        Failure::new(ErrorCode::InvalidInput, error.message).at(error.path)
    }
}

impl From<InputError> for PackError {
    fn from(error: InputError) -> Self {
        Self::new(PackErrorKind::InvalidField, error.path, error.message)
    }
}

/// The JSON Schema of a decoded pack (TOML tables map to JSON objects).
pub fn schema() -> Schema {
    schema_for!(MethodPack)
}

/// Parse and validate a pack from its file bytes without side effects.
pub fn load(bytes: &[u8]) -> Result<LoadedPack, PackError> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(PackError::new(
            PackErrorKind::TooLarge,
            "",
            "Method pack exceeds its byte limit",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| PackError::new(PackErrorKind::InvalidUtf8, "", "Method pack must be UTF-8"))?;
    let table: toml::Table = toml::from_str(text).map_err(|error| PackError {
        line: error.span().map(|span| {
            1 + text.as_bytes()[..span.start]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
        }),
        ..PackError::new(
            PackErrorKind::InvalidToml,
            "",
            "Malformed TOML; duplicate keys and tables are rejected",
        )
    })?;
    let mut segments = Vec::new();
    let value = convert_table(table, &mut segments)?;
    match value.get("schema_version") {
        Some(Value::String(version)) if version == SCHEMA_VERSION => {}
        Some(Value::String(_)) => {
            return Err(PackError::new(
                PackErrorKind::UnsupportedVersion,
                "/schema_version",
                "Unsupported method pack version; expected inv.method-pack.v1",
            ));
        }
        _ => {
            return Err(PackError::new(
                PackErrorKind::InvalidField,
                "/schema_version",
                "Method pack requires schema_version = \"inv.method-pack.v1\"",
            ));
        }
    }
    let pack: MethodPack = input::decode(&value)?;
    validate(&pack)?;
    Ok(LoadedPack {
        pack,
        digest: format!("sha256:{:x}", Sha256::digest(bytes))
            .try_into()
            .expect("SHA-256 hex is a valid digest"),
        byte_len: bytes.len(),
    })
}

/// Read at most [`MAX_PACK_BYTES`] + 1 bytes from `path`, then [`load`] them.
/// This is the only I/O in the module: one read-only open.
pub fn load_file(path: &Path) -> Result<LoadedPack, PackError> {
    let unreadable = |_| {
        PackError::new(
            PackErrorKind::Unreadable,
            "",
            "Cannot read the method pack file",
        )
    };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let mut bytes = Vec::new();
    file.take(MAX_PACK_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    load(&bytes)
}

/// The built-in `general` pack. Its bytes are part of the binary.
pub fn builtin_general() -> LoadedPack {
    load(GENERAL_PACK_SOURCE).expect("the built-in general pack is validated by tests")
}

fn convert_table(table: toml::Table, segments: &mut Vec<String>) -> Result<Value, PackError> {
    let mut object = Map::new();
    for (key, value) in table {
        segments.push(key);
        let key = segments.last().expect("pushed");
        if executable_key(key) {
            return Err(PackError::new(
                PackErrorKind::ExecutableField,
                pointer(segments.iter()),
                "Method packs are data; fields for hooks, commands, code, includes or remote references are not allowed",
            ));
        }
        let converted = convert(value, segments)?;
        let key = segments.pop().expect("pushed");
        object.insert(key, converted);
    }
    Ok(Value::Object(object))
}

fn convert(value: toml::Value, segments: &mut Vec<String>) -> Result<Value, PackError> {
    if segments.len() > MAX_PACK_DEPTH {
        return Err(PackError::new(
            PackErrorKind::TooDeep,
            pointer(segments.iter()),
            "Method pack exceeds its nesting limit",
        ));
    }
    const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;
    Ok(match value {
        toml::Value::String(text) => Value::String(text),
        toml::Value::Boolean(flag) => Value::Bool(flag),
        toml::Value::Integer(number)
            if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&number) =>
        {
            Value::Number(number.into())
        }
        toml::Value::Integer(_) => {
            return Err(PackError::new(
                PackErrorKind::UnsafeNumber,
                pointer(segments.iter()),
                "Integers must be within +/-(2^53 - 1)",
            ));
        }
        toml::Value::Float(_) | toml::Value::Datetime(_) => {
            return Err(PackError::new(
                PackErrorKind::UnsupportedValue,
                pointer(segments.iter()),
                "Method packs accept strings, integers, booleans, arrays and tables only",
            ));
        }
        toml::Value::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for (index, item) in items.into_iter().enumerate() {
                segments.push(index.to_string());
                let converted = convert(item, segments);
                segments.pop();
                values.push(converted?);
            }
            Value::Array(values)
        }
        toml::Value::Table(table) => convert_table(table, segments)?,
    })
}

/// Keys that would suggest the pack can run, load or fetch something. Every
/// key in the schema is structural (names are values, never keys), so this
/// cannot reject an author-chosen property name.
fn executable_key(key: &str) -> bool {
    const EXACT: &[&str] = &[
        "action",
        "args",
        "argv",
        "binary",
        "callback",
        "cmd",
        "code",
        "command",
        "commands",
        "endpoint",
        "entrypoint",
        "env",
        "eval",
        "exec",
        "executable",
        "execute",
        "fetch",
        "handler",
        "hook",
        "hooks",
        "href",
        "import",
        "imports",
        "include",
        "includes",
        "load",
        "module",
        "modules",
        "plugin",
        "plugins",
        "program",
        "run",
        "runs",
        "script",
        "scripts",
        "shell",
        "uri",
        "url",
        "urls",
        "wasm",
    ];
    const PREFIXES: &[&str] = &["$", "on_", "pre_", "post_", "exec_", "run_"];
    const SUFFIXES: &[&str] = &[
        "_cmd",
        "_command",
        "_commands",
        "_exec",
        "_hook",
        "_hooks",
        "_script",
        "_url",
        "_uri",
    ];
    let key = key.to_ascii_lowercase().replace('-', "_");
    EXACT.contains(&key.as_str())
        || PREFIXES.iter().any(|prefix| key.starts_with(prefix))
        || SUFFIXES.iter().any(|suffix| key.ends_with(suffix))
}

struct Check {
    segments: Vec<String>,
}

impl Check {
    /// The current path plus `field`; an empty `field` names the current value.
    fn path(&self, field: &str) -> String {
        pointer(
            self.segments
                .iter()
                .map(String::as_str)
                .chain(Some(field).filter(|field| !field.is_empty())),
        )
    }

    fn fail(&self, kind: PackErrorKind, field: &str, message: &'static str) -> PackError {
        PackError::new(kind, self.path(field), message)
    }

    fn within<T>(
        &mut self,
        segments: &[&str],
        check: impl FnOnce(&mut Self) -> Result<T, PackError>,
    ) -> Result<T, PackError> {
        let depth = self.segments.len();
        self.segments
            .extend(segments.iter().map(|s| (*s).to_owned()));
        let result = check(self);
        self.segments.truncate(depth);
        result
    }

    fn each<T>(
        &mut self,
        field: &str,
        items: &[T],
        mut check: impl FnMut(&mut Self, &T) -> Result<(), PackError>,
    ) -> Result<(), PackError> {
        if items.len() > MAX_ENTRIES {
            return Err(self.fail(PackErrorKind::TooLarge, field, "List has too many entries"));
        }
        for (index, item) in items.iter().enumerate() {
            self.within(&[field, &index.to_string()], |this| check(this, item))?;
        }
        Ok(())
    }

    fn name(&self, field: &str, value: &str) -> Result<(), PackError> {
        self.identifier(field, value, '-', false)
    }

    fn identifier(
        &self,
        field: &str,
        value: &str,
        separator: char,
        dotted: bool,
    ) -> Result<(), PackError> {
        let valid_segment = |segment: &str| {
            let mut bytes = segment.bytes();
            bytes.next().is_some_and(|b| b.is_ascii_lowercase())
                && !segment.ends_with(separator)
                && !segment.contains(&format!("{separator}{separator}"))
                && bytes
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == separator as u8)
        };
        let valid = value.len() <= MAX_NAME_BYTES
            && if dotted {
                value.split('.').all(valid_segment)
            } else {
                valid_segment(value)
            };
        if valid {
            Ok(())
        } else {
            Err(self.fail(
                PackErrorKind::InvalidName,
                field,
                "Names are short lowercase identifiers, never paths or URLs",
            ))
        }
    }

    fn line(&self, field: &str, value: &str) -> Result<(), PackError> {
        self.text_with(field, value, MAX_LINE_BYTES, false)
    }

    fn text(&self, field: &str, value: &str) -> Result<(), PackError> {
        self.text_with(field, value, MAX_TEXT_BYTES, true)
    }

    fn text_with(
        &self,
        field: &str,
        value: &str,
        limit: usize,
        multiline: bool,
    ) -> Result<(), PackError> {
        if value.len() > limit {
            return Err(self.fail(
                PackErrorKind::TextTooLong,
                field,
                "Text exceeds its byte limit",
            ));
        }
        if value.trim().is_empty() {
            return Err(self.fail(PackErrorKind::InvalidText, field, "Text must not be blank"));
        }
        if value
            .chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\t')))
        {
            return Err(self.fail(
                PackErrorKind::InvalidText,
                field,
                "Text contains a control character",
            ));
        }
        let lower = value.to_ascii_lowercase();
        if ["{{", "{%", "${", "<%", "<?", "<script"]
            .iter()
            .any(|marker| lower.contains(marker))
        {
            return Err(self.fail(
                PackErrorKind::InvalidText,
                field,
                "Pack text is plain prose; template or script markup is not allowed",
            ));
        }
        Ok(())
    }

    fn unique<'a>(
        &self,
        field: &str,
        values: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), PackError> {
        let mut seen = BTreeSet::new();
        for (index, value) in values.into_iter().enumerate() {
            if !seen.insert(value) {
                return Err(PackError::new(
                    PackErrorKind::Duplicate,
                    pointer(
                        self.segments
                            .iter()
                            .map(String::as_str)
                            .chain([field, &index.to_string()]),
                    ),
                    "Duplicate entry",
                ));
            }
        }
        Ok(())
    }

    fn modes(
        &self,
        field: &str,
        modes: &[CaseMode],
        allowed: Option<&[CaseMode]>,
    ) -> Result<(), PackError> {
        let names: Vec<String> = modes.iter().map(|mode| format!("{mode:?}")).collect();
        self.unique(field, names.iter().map(String::as_str))?;
        if let Some(allowed) = allowed
            && let Some(index) = modes.iter().position(|mode| !allowed.contains(mode))
        {
            return Err(PackError::new(
                PackErrorKind::UnknownReference,
                pointer(
                    self.segments
                        .iter()
                        .map(String::as_str)
                        .chain([field, &index.to_string()]),
                ),
                "Mode is not one of the pack's modes",
            ));
        }
        Ok(())
    }
}

fn validate(pack: &MethodPack) -> Result<(), PackError> {
    let mut check = Check {
        segments: Vec::new(),
    };
    check.name("id", &pack.id)?;
    if !semantic_version(&pack.version) {
        return Err(check.fail(
            PackErrorKind::InvalidName,
            "version",
            "Version is MAJOR.MINOR.PATCH without leading zeroes",
        ));
    }
    check.line("title", &pack.title)?;
    check.line("summary", &pack.summary)?;
    if pack.modes.is_empty() {
        return Err(check.fail(
            PackErrorKind::InvalidField,
            "modes",
            "List at least one case mode",
        ));
    }
    check.modes("modes", &pack.modes, None)?;

    let catalog = standard_catalog();
    let active: BTreeSet<&str> = catalog
        .operations()
        .filter(|op| op.status() == Status::Active)
        .map(|op| op.info().name)
        .collect();
    let stages: BTreeSet<&str> = catalog
        .operations()
        .filter(|op| op.status() == Status::Active)
        .map(|op| op.info().stage)
        .collect();

    check.unique("cards", pack.cards.iter().map(|card| card.name.as_str()))?;
    check.each("cards", &pack.cards, |check, card| {
        check.name("name", &card.name)?;
        check.line("title", &card.title)?;
        if !stages.contains(card.stage.as_str()) {
            return Err(check.fail(
                PackErrorKind::UnknownReference,
                "stage",
                "Stage is not a workflow stage of the operation catalog",
            ));
        }
        check.line("summary", &card.summary)?;
        check.modes("modes", &card.modes, Some(&pack.modes))?;
        check.each("triggers", &card.triggers, |check, trigger| {
            validate_trigger(check, trigger)
        })?;
        let triggers: Vec<String> = card
            .triggers
            .iter()
            .map(|trigger| serde_json::to_string(trigger).expect("trigger serializes"))
            .collect();
        check.unique("triggers", triggers.iter().map(String::as_str))?;
        check.unique("operations", card.operations.iter().map(String::as_str))?;
        check.each("operations", &card.operations, |check, operation| {
            if active.contains(operation.as_str()) {
                Ok(())
            } else {
                Err(check.fail(
                    PackErrorKind::UnknownReference,
                    "",
                    "Operation is not active in the operation catalog",
                ))
            }
        })?;
        check.within(&["body"], |check| {
            let body = &card.body;
            check.text("when", &body.when)?;
            check.text("record", &body.record)?;
            if let Some(mistake) = &body.mistake {
                check.text("mistake", mistake)?;
            }
            check.text("skip", &body.skip)?;
            let total = body.when.len()
                + body.record.len()
                + body.mistake.as_ref().map_or(0, String::len)
                + body.skip.len();
            if total > MAX_CARD_BODY_BYTES {
                return Err(PackError::new(
                    PackErrorKind::TextTooLong,
                    pointer(check.segments.iter()),
                    "Card body exceeds its byte limit; keep each card to one screen",
                ));
            }
            Ok(())
        })
    })?;

    check.unique(
        "properties",
        pack.properties.iter().map(|p| p.name.as_str()),
    )?;
    check.each("properties", &pack.properties, |check, property| {
        check.identifier("name", &property.name, '_', false)?;
        if property.record_kinds.is_empty() {
            return Err(check.fail(
                PackErrorKind::InvalidField,
                "record_kinds",
                "List at least one record kind",
            ));
        }
        record_kinds(check, "record_kinds", &property.record_kinds)?;
        check.text("description", &property.description)?;
        check.within(&["schema"], |check| {
            validate_value_schema(check, &property.schema)
        })
    })?;

    check.unique(
        "observation_guidance",
        pack.observation_guidance.iter().map(|g| g.name.as_str()),
    )?;
    check.each(
        "observation_guidance",
        &pack.observation_guidance,
        |check, guidance| {
            check.name("name", &guidance.name)?;
            check.line("title", &guidance.title)?;
            check.text("text", &guidance.text)?;
            check.modes("modes", &guidance.modes, Some(&pack.modes))
        },
    )?;

    check.unique("policies", pack.policies.iter().map(|p| p.name.as_str()))?;
    check.each("policies", &pack.policies, |check, policy| {
        check.name("name", &policy.name)?;
        if !POLICY_GATES.contains(&policy.gate.as_str()) || !active.contains(policy.gate.as_str()) {
            return Err(check.fail(
                PackErrorKind::UnknownReference,
                "gate",
                "Policies may only add requirements to promotion or closure operations",
            ));
        }
        if !policy.input.gates().contains(&policy.gate.as_str()) {
            return Err(check.fail(
                PackErrorKind::UnknownReference,
                "gate",
                "This policy input does not apply at that gate",
            ));
        }
        if policy.input == PolicyInput::ClosureRetrospective {
            check.each("triggers", &policy.triggers, |check, trigger| {
                validate_trigger(check, trigger)?;
                if trigger.gap_rule.is_some() {
                    return Err(check.fail(
                        PackErrorKind::InvalidField,
                        "gap_rule",
                        "Closure triggers name a case fact or record kind",
                    ));
                }
                Ok(())
            })?;
            let triggers: Vec<String> = policy
                .triggers
                .iter()
                .map(|trigger| serde_json::to_string(trigger).expect("trigger serializes"))
                .collect();
            check.unique("triggers", triggers.iter().map(String::as_str))?;
        } else if !policy.triggers.is_empty() {
            return Err(check.fail(
                PackErrorKind::InvalidField,
                "triggers",
                "Only closure_retrospective takes triggers",
            ));
        }
        match (policy.input, policy.fields.is_empty()) {
            (PolicyInput::RequiredFields, true) => {
                return Err(check.fail(
                    PackErrorKind::InvalidField,
                    "fields",
                    "required_fields lists at least one field",
                ));
            }
            (PolicyInput::RequiredFields, false) => {}
            (_, false) => {
                return Err(check.fail(
                    PackErrorKind::InvalidField,
                    "fields",
                    "Only required_fields takes a field list",
                ));
            }
            (_, true) => {}
        }
        check.unique("fields", policy.fields.iter().map(String::as_str))?;
        check.each("fields", &policy.fields, |check, field| {
            if field_path(field) {
                Ok(())
            } else {
                Err(check.fail(
                    PackErrorKind::InvalidName,
                    "",
                    "Fields are dotted lowercase payload names",
                ))
            }
        })?;
        check.text("rationale", &policy.rationale)
    })?;

    check.unique("views", pack.views.iter().map(|v| v.name.as_str()))?;
    check.each("views", &pack.views, |check, view| {
        check.name("name", &view.name)?;
        check.line("title", &view.title)?;
        check.identifier("view", &view.view, '-', true)?;
        record_kinds(check, "record_kinds", &view.record_kinds)
    })?;

    check.unique("reports", pack.reports.iter().map(|r| r.name.as_str()))?;
    check.each("reports", &pack.reports, |check, report| {
        check.name("name", &report.name)?;
        check.line("title", &report.title)?;
        check.identifier("template", &report.template, '-', true)
    })
}

fn validate_trigger(check: &mut Check, trigger: &Trigger) -> Result<(), PackError> {
    let set = usize::from(trigger.gap_rule.is_some())
        + usize::from(trigger.case_fact.is_some())
        + usize::from(trigger.record_kind.is_some());
    if set != 1 {
        return Err(PackError::new(
            PackErrorKind::InvalidField,
            pointer(check.segments.iter()),
            "A trigger names exactly one of gap_rule, case_fact or record_kind",
        ));
    }
    if let Some(rule) = &trigger.gap_rule
        && !GAP_RULE_TRIGGERS.contains(&rule.as_str())
    {
        return Err(check.fail(
            PackErrorKind::UnknownReference,
            "gap_rule",
            "Trigger must name a built-in gap rule; packs cannot define predicates",
        ));
    }
    Ok(())
}

fn record_kinds(check: &Check, field: &str, kinds: &[RecordKind]) -> Result<(), PackError> {
    let names: Vec<String> = kinds.iter().map(|kind| format!("{kind:?}")).collect();
    check.unique(field, names.iter().map(String::as_str))
}

fn validate_value_schema(check: &mut Check, schema: &ValueSchema) -> Result<(), PackError> {
    if let Some(description) = &schema.description {
        check.text("description", description)?;
    }
    let present = [
        ("enum", schema.allowed.is_some()),
        ("max_length", schema.max_length.is_some()),
        ("minimum", schema.minimum.is_some()),
        ("maximum", schema.maximum.is_some()),
        ("items", schema.items.is_some()),
        ("max_items", schema.max_items.is_some()),
        ("fields", schema.fields.is_some()),
        ("required", schema.required.is_some()),
    ];
    let (permitted, mandatory): (&[&str], &[&str]) = match schema.value_type {
        ValueType::String => (&["enum", "max_length"], &["max_length"]),
        ValueType::Integer => (&["minimum", "maximum"], &[]),
        ValueType::Decimal | ValueType::Boolean => (&[], &[]),
        ValueType::Array => (&["items", "max_items"], &["items", "max_items"]),
        ValueType::Object => (&["fields", "required"], &["fields"]),
    };
    for (keyword, is_present) in present {
        if is_present && !permitted.contains(&keyword) {
            return Err(check.fail(
                PackErrorKind::InvalidSchema,
                keyword,
                "Keyword does not apply to this property type",
            ));
        }
        if !is_present && mandatory.contains(&keyword) {
            return Err(check.fail(
                PackErrorKind::InvalidSchema,
                keyword,
                "Keyword is required for this property type",
            ));
        }
    }
    if let Some(max_length) = schema.max_length
        && !(1..=MAX_PROPERTY_STRING_BYTES).contains(&max_length)
    {
        return Err(check.fail(
            PackErrorKind::InvalidSchema,
            "max_length",
            "max_length is out of range",
        ));
    }
    if let Some(allowed) = &schema.allowed {
        if allowed.is_empty() {
            return Err(check.fail(
                PackErrorKind::InvalidSchema,
                "enum",
                "List at least one value",
            ));
        }
        check.unique("enum", allowed.iter().map(String::as_str))?;
        check.each("enum", allowed, |check, value| {
            check.line("", value)?;
            if schema
                .max_length
                .is_some_and(|max| value.len() > max as usize)
            {
                return Err(check.fail(
                    PackErrorKind::InvalidSchema,
                    "",
                    "Enumerated value exceeds max_length",
                ));
            }
            Ok(())
        })?;
    }
    if let (Some(minimum), Some(maximum)) = (schema.minimum, schema.maximum)
        && minimum > maximum
    {
        return Err(check.fail(
            PackErrorKind::InvalidSchema,
            "minimum",
            "minimum exceeds maximum",
        ));
    }
    if let Some(max_items) = schema.max_items
        && !(1..=MAX_PROPERTY_ITEMS).contains(&max_items)
    {
        return Err(check.fail(
            PackErrorKind::InvalidSchema,
            "max_items",
            "max_items is out of range",
        ));
    }
    if let Some(items) = &schema.items {
        check.within(&["items"], |check| validate_value_schema(check, items))?;
    }
    if let Some(fields) = &schema.fields {
        if fields.is_empty() {
            return Err(check.fail(
                PackErrorKind::InvalidSchema,
                "fields",
                "List at least one field",
            ));
        }
        check.unique("fields", fields.iter().map(|field| field.name.as_str()))?;
        check.each("fields", fields, |check, field| {
            check.identifier("name", &field.name, '_', false)?;
            check.within(&["schema"], |check| {
                validate_value_schema(check, &field.schema)
            })
        })?;
        if let Some(required) = &schema.required {
            check.unique("required", required.iter().map(String::as_str))?;
            if let Some(index) = required
                .iter()
                .position(|name| !fields.iter().any(|field| &field.name == name))
            {
                return Err(PackError::new(
                    PackErrorKind::UnknownReference,
                    pointer(
                        check
                            .segments
                            .iter()
                            .map(String::as_str)
                            .chain(["required", &index.to_string()]),
                    ),
                    "Required field is not declared in fields",
                ));
            }
        }
    }
    Ok(())
}

fn field_path(value: &str) -> bool {
    value.len() <= 2 * MAX_NAME_BYTES
        && value.split('.').all(|segment| {
            let mut bytes = segment.bytes();
            bytes.next().is_some_and(|b| b.is_ascii_lowercase())
                && !segment.ends_with(['-', '_'])
                && bytes
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
}

fn semantic_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 9
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

#[cfg(test)]
mod tests;
