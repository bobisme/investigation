//! `capture.begin`: declare one Run and obtain a capture handle.

use std::path::PathBuf;

use investigation_core::{
    command::RevisionRef,
    header::{Actor, ClassificationLevel},
    identity::{Id, Kind},
    wire::{Digest, ErrorCode, Failure, Receipt},
};
use investigation_store::{
    Store, Workspace,
    store::capture::{CaptureDeclaration, DeclarationTarget, HandleLimits},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    error::RecordingError,
    key::{
        CaptureHandle, CaptureVersion, ExternalRunId, IdempotencyKey, Metadata, ProducerIdentity,
    },
};

/// Request to declare one Run before the producer acts.
///
/// Idempotent on the producer name, external run ID and key: a retry after a lost
/// response returns the same Run and handle. A different key or actor for an
/// already declared producer name and external run ID is refused with
/// `IDEMPOTENCY_KEY_REUSED`, naming the original command ID.
///
/// Declaring a Run records the producer's intent. It does not authorize, approve
/// or start anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BeginCapture {
    /// Journal command ID for the declaring transaction.
    pub key: IdempotencyKey,
    /// Actor attributed with the declaration and every later call on the handle.
    pub actor: Actor,
    /// Self-reported producer identity.
    pub producer: ProducerIdentity,
    /// The producer's identifier for this logical attempt.
    pub external_run_id: ExternalRunId,
    /// What the producer intends the Run to do.
    pub intent: CaptureIntent,
    /// Case that receives the Run.
    pub case: CaseTarget,
    /// Default classification for retained items; an item may request a stricter one.
    #[serde(default)]
    pub classification: ClassificationLevel,
    /// Inert namespaced producer metadata.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
}

impl BeginCapture {
    /// A request with `internal` classification and no metadata.
    pub fn new(
        key: IdempotencyKey,
        actor: Actor,
        producer: ProducerIdentity,
        external_run_id: ExternalRunId,
        intent: CaptureIntent,
        case: CaseTarget,
    ) -> Self {
        Self {
            key,
            actor,
            producer,
            external_run_id,
            intent,
            case,
            classification: ClassificationLevel::default(),
            metadata: Metadata::new(),
        }
    }
}

/// The producer's stated intent. It is recorded, never executed or approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CaptureIntent {
    /// Short human-readable purpose.
    pub label: String,
    /// Exact revision of an InquiryPlan in the target case, when the Run follows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inquiry_plan: Option<RevisionRef>,
    /// Digest of a plan held outside Investigation, when the producer has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_plan_digest: Option<Digest>,
}

impl CaptureIntent {
    /// An exploratory intent with no plan reference.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            inquiry_plan: None,
            external_plan_digest: None,
        }
    }
}

/// Case that receives the Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum CaseTarget {
    /// Record into an existing case.
    Existing {
        /// The case identifier.
        case_id: Id,
    },
    /// Create a minimal `record`-mode case in the same transaction as the Run.
    ///
    /// The case has no question, hypothesis or decision, and needs no vault.
    NewRecordCase {
        /// Case title.
        title: String,
    },
}

/// Limits the recorder enforces on one handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CaptureLimits {
    /// Largest accepted serialized `append` request, in bytes.
    pub max_event_bytes: u64,
    /// Largest accepted attachment, in bytes.
    pub max_attachment_bytes: u64,
    /// Largest accepted position on one handle.
    pub max_items: u64,
}

impl CaptureLimits {
    /// Limits with the given values.
    pub const fn new(max_event_bytes: u64, max_attachment_bytes: u64, max_items: u64) -> Self {
        Self {
            max_event_bytes,
            max_attachment_bytes,
            max_items,
        }
    }
}

/// Durable receipt for a declared Run.
///
/// The receipt records a declaration only. It does not authorize, approve or
/// start the producer's action; the producer decides that under its own policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BeginReceipt {
    /// Handle for every later call on this capture.
    pub handle: CaptureHandle,
    /// The declared Run.
    pub run_id: Id,
    /// The case holding the Run.
    pub case_id: Id,
    /// Negotiated protocol version.
    pub protocol: CaptureVersion,
    /// Limits for this handle.
    pub limits: CaptureLimits,
    /// Private durable staging directory owned by the recorder for this handle,
    /// relative to the workspace root (`state/staging/<run-id>`).
    ///
    /// The recorder copies attachments into it. Producers must not write into it or
    /// link files from it; it is recovery material, not a cache.
    pub staging_dir: PathBuf,
    /// Journal receipt of the declaring transaction. On a retry its result is
    /// `already_committed` and every identifier is unchanged.
    pub commit: Receipt,
}

const MAX_LABEL_CHARS: usize = 1024;
const MAX_TITLE_CHARS: usize = 256;
const MAX_IDENTITY_CHARS: usize = 128;

/// Declare the Run in one transaction, then allocate its staging directory.
pub(crate) fn declare(
    workspace: &Workspace,
    request: &BeginCapture,
) -> Result<BeginReceipt, RecordingError> {
    validate(request)?;
    let declaration = declaration(request)?;
    let declared = Store::open(workspace.clone())
        .declare_capture(&declaration)
        .map_err(request_path)?;
    let invalid_handle = || {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "The committed capture handle is not a handle identifier.",
        )
    };
    let HandleLimits {
        max_event_bytes,
        max_attachment_bytes,
        max_items,
    } = declared.binding.limits;
    Ok(BeginReceipt {
        handle: CaptureHandle::from_id(declared.binding.handle).map_err(|_| invalid_handle())?,
        run_id: declared.run_id,
        case_id: declared.case_id,
        protocol: CaptureVersion::V1,
        limits: CaptureLimits::new(max_event_bytes, max_attachment_bytes, max_items),
        staging_dir: PathBuf::from(declared.binding.staging),
        commit: declared.receipt,
    })
}

fn declaration(request: &BeginCapture) -> Result<CaptureDeclaration, Failure> {
    let encoding = || {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Could not encode the begin request.",
        )
    };
    let intent = match &request.intent.inquiry_plan {
        Some(plan_ref) => json!({"kind": "plan", "plan_ref": plan_ref}),
        None => json!({"kind": "exploration", "intent": request.intent.label}),
    };
    let producer = &request.producer;
    let tool = match &producer.version {
        Some(version) => format!("{} {version}", producer.name),
        None => producer.name.to_string(),
    };
    Ok(CaptureDeclaration {
        command_id: request.key.as_str().to_owned(),
        actor: request.actor.clone(),
        target: match &request.case {
            CaseTarget::Existing { case_id } => DeclarationTarget::Existing(*case_id),
            CaseTarget::NewRecordCase { title } => DeclarationTarget::NewRecordCase {
                title: title.clone(),
            },
        },
        run: json!({
            "intent": intent,
            "producer": request.actor,
            "tool": tool,
            "external_ref": request.external_run_id,
            "evidence_kind": "observational",
        }),
        producer: producer.name.to_string(),
        external_run_id: request.external_run_id.to_string(),
        external_plan_digest: request.intent.external_plan_digest.clone(),
        classification: request.classification,
        // The normalized form, so an identical retry compares equal however
        // its JSON was spelled.
        request: serde_json::to_value(request).map_err(|_| encoding())?,
    })
}

fn validate(request: &BeginCapture) -> Result<(), Failure> {
    let invalid = |path: &str, message: String| {
        Failure::new(ErrorCode::InvalidInput, message).at(path.to_owned())
    };
    let label = &request.intent.label;
    if label.trim().is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(invalid(
            "/intent/label",
            format!("Describe the intent in 1 to {MAX_LABEL_CHARS} characters."),
        ));
    }
    for (path, value) in [
        ("/producer/version", &request.producer.version),
        ("/producer/instance", &request.producer.instance),
    ] {
        if value.as_ref().is_some_and(|value| {
            value.trim().is_empty()
                || value.chars().count() > MAX_IDENTITY_CHARS
                || value.chars().any(char::is_control)
        }) {
            return Err(invalid(
                path,
                format!(
                    "Use 1 to {MAX_IDENTITY_CHARS} characters without control characters, or omit the field."
                ),
            ));
        }
    }
    match &request.case {
        CaseTarget::Existing { case_id } if case_id.kind() != Kind::Case => {
            return Err(invalid(
                "/case/case_id",
                "Use a case identifier with the case_ prefix.".into(),
            ));
        }
        CaseTarget::NewRecordCase { title }
            if title.trim().is_empty() || title.chars().count() > MAX_TITLE_CHARS =>
        {
            return Err(invalid(
                "/case/title",
                format!("Give the new case a title of 1 to {MAX_TITLE_CHARS} characters."),
            ));
        }
        CaseTarget::NewRecordCase { .. } if request.intent.inquiry_plan.is_some() => {
            return Err(invalid(
                "/intent/inquiry_plan",
                "A new record case holds no inquiry plan. Target the existing case that holds the frozen plan.".into(),
            ));
        }
        _ => {}
    }
    investigation_store::opaque::validate_metadata(&request.metadata)
        .map_err(|(message, key)| invalid(&format!("/metadata{key}"), message))
}

/// Report store failures against this request's fields.
fn request_path(mut failure: Failure) -> RecordingError {
    let mapped = failure.error.path.as_deref().map(|path| {
        [
            ("/run/intent/plan_ref", "/intent/inquiry_plan"),
            ("/run/intent/intent", "/intent/label"),
            ("/run/intent", "/intent"),
            ("/run/external_ref", "/external_run_id"),
            ("/run/producer", "/actor"),
            ("/case_id", "/case/case_id"),
        ]
        .into_iter()
        .find_map(|(from, to)| path.strip_prefix(from).map(|rest| format!("{to}{rest}")))
        .unwrap_or_else(|| path.to_owned())
    });
    failure.error.path = mapped;
    RecordingError::Rejected(failure)
}
