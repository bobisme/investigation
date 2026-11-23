//! What this build of the capture protocol supports, for producers to read.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{begin::CaptureLimits, key::CaptureVersion, key::Operation};

/// Version tag of the capabilities document.
pub const CAPABILITIES_VERSION: &str = "inv.capture-capabilities.v1";

/// The capture protocol versions, operations and limits this build supports.
///
/// Every value comes from the code that enforces it, so the document cannot
/// advertise anything the build lacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CaptureCapabilities {
    /// `inv.capture-capabilities.v1`.
    pub schema_version: String,
    /// Protocol versions this build accepts, newest first.
    pub protocols: Vec<String>,
    /// Version a `begin` without `protocol` negotiates.
    pub default_protocol: String,
    /// Wire names of the capture operations, in lifecycle order.
    pub operations: Vec<String>,
    /// Limits every handle declared by this build receives, fixed at `begin`.
    pub limits: CaptureLimits,
    /// Versions of the one-document completed-run envelope this build accepts.
    pub producer_run_versions: Vec<String>,
}

impl CaptureCapabilities {
    /// The capabilities of this build.
    pub fn current() -> Self {
        Self {
            schema_version: CAPABILITIES_VERSION.to_owned(),
            protocols: CaptureVersion::SUPPORTED
                .iter()
                .map(|version| version.as_str().to_owned())
                .collect(),
            default_protocol: CaptureVersion::V1.as_str().to_owned(),
            operations: Operation::ALL
                .iter()
                .map(|operation| operation.wire_name().to_owned())
                .collect(),
            limits: CaptureLimits::current(),
            producer_run_versions: vec!["inv.producer-run.v1".to_owned()],
        }
    }
}
