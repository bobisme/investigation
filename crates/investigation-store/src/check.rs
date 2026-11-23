use std::{collections::BTreeSet, io::Read};

use investigation_core::{
    evidence::{ArtifactAvailability, ArtifactBody},
    wire::{Digest, ProjectionStatus},
};
use sha2::{Digest as _, Sha256};

use super::{capture::retire::StagingSummary, *};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckIssue {
    pub severity: Severity,
    pub code: String,
    pub objects: Vec<Id>,
    pub reason: String,
    pub repair: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    pub heads: Vec<CaseHead>,
    pub structurally_valid: bool,
    pub verified_blobs: usize,
    pub unavailable_artifacts: usize,
    /// Of `unavailable_artifacts`, those recorded as references to bytes held
    /// elsewhere. They are never counted as verified blobs: `check` reads
    /// only retained bytes and never fetches a locator.
    pub reference_only_artifacts: usize,
    pub source_authenticated: Option<bool>,
    pub independently_anchored: Option<bool>,
    pub index: ProjectionStatus,
    /// Capture staging by state; see `capture::retire`.
    pub staging: StagingSummary,
    pub issues: Vec<CheckIssue>,
}

impl CheckReport {
    pub fn integrity_ok(&self) -> bool {
        self.structurally_valid
            && self
                .issues
                .iter()
                .all(|issue| issue.severity != Severity::Error)
    }
}

#[derive(Default)]
struct BlobClaims {
    objects: BTreeSet<Id>,
    lengths: BTreeSet<u64>,
}

impl<F: DurableFs> Store<F> {
    pub fn check(&mut self) -> Result<CheckReport, Failure> {
        let _lock = self.coordinate()?;
        let mut report = CheckReport {
            heads: Vec::new(),
            structurally_valid: false,
            verified_blobs: 0,
            unavailable_artifacts: 0,
            reference_only_artifacts: 0,
            source_authenticated: None,
            independently_anchored: None,
            index: ProjectionStatus::Pending,
            staging: StagingSummary::default(),
            issues: Vec::new(),
        };
        let verified = match self.load_verified() {
            Ok(replay) => replay,
            Err(error) => {
                report.issues.push(CheckIssue {
                    severity: Severity::Error,
                    code: error
                        .error
                        .actual
                        .as_ref()
                        .and_then(|actual| actual.get("diagnostic_code"))
                        .and_then(Value::as_str)
                        .unwrap_or("JOURNAL_UNVERIFIED")
                        .to_owned(),
                    objects: Vec::new(),
                    reason: error.error.message.clone(),
                    repair: None,
                });
                return Ok(report);
            }
        };
        report.structurally_valid = true;
        report.heads = verified.heads();
        self.check_blobs(&verified, &mut report);
        self.check_staging(&verified, &mut report);
        let expected_heads: Vec<_> = report
            .heads
            .iter()
            .map(|head| (head.case_id, head.snapshot.clone()))
            .collect();
        if Projection::inspect(&self.index_path(), self.workspace.id()).as_ref()
            == Ok(&expected_heads)
        {
            report.index = ProjectionStatus::Current;
        } else {
            report.index = ProjectionStatus::Degraded;
            report.issues.push(CheckIssue {
                severity: Severity::Warning,
                code: "INDEX_STALE".into(),
                objects: Vec::new(),
                reason: "The disposable index is absent or does not match the verified case heads."
                    .into(),
                repair: Some(vec![
                    PROGRAM_NAME.into(),
                    "repair".into(),
                    "index".into(),
                    "--format".into(),
                    "json".into(),
                ]),
            });
        }
        if self.fs.read(Path::new(RECOVERY_MARKER)).is_ok() {
            report.issues.push(CheckIssue {
                severity: Severity::Warning,
                code: "PRIOR_SYNC_FAILURE".into(),
                objects: Vec::new(),
                reason: "A previous writer stopped after a filesystem sync failure. The current heads were checked from canonical files.".into(),
                repair: None,
            });
        }
        Ok(report)
    }

    fn check_blobs(&self, workspace: &WorkspaceReplay, report: &mut CheckReport) {
        let mut blobs = BTreeMap::<Digest, BlobClaims>::new();
        for case in workspace.cases.values() {
            for verified in &case.events {
                for digest in verified
                    .event
                    .request
                    .attachments
                    .iter()
                    .chain(verified.event.manifest_digest.iter())
                {
                    blobs
                        .entry(digest.clone())
                        .or_default()
                        .objects
                        .insert(verified.event.transaction_id);
                }
            }
            for record in case
                .state
                .records()
                .filter(|record| record.header.id.kind() == Kind::Artifact)
            {
                match investigation_core::input::decode::<ArtifactBody>(&record.body) {
                    Ok(body) if body.availability != ArtifactAvailability::Retained => {
                        report.unavailable_artifacts += 1;
                        if body.availability == ArtifactAvailability::External {
                            report.reference_only_artifacts += 1;
                        }
                    }
                    Ok(body) => match (body.digest, body.size_bytes) {
                        (Some(digest), Some(length)) => {
                            let claim = blobs.entry(digest).or_default();
                            claim.objects.insert(record.header.id);
                            claim.lengths.insert(length);
                        }
                        _ => invalid_artifact(report, record.header.id),
                    },
                    Err(_) => invalid_artifact(report, record.header.id),
                }
            }
        }
        for (digest, claim) in blobs {
            let hex = &digest.as_str()["sha256:".len()..];
            let path = Path::new(BLOBS).join(&hex[..2]).join(hex);
            let failure = match self.fs.read(&path).and_then(read_hash) {
                Ok((actual, size))
                    if actual == hex && claim.lengths.iter().all(|length| *length == size) =>
                {
                    report.verified_blobs += 1;
                    continue;
                }
                Ok(_) => (
                    "BLOB_MISMATCH",
                    "Stored bytes differ from their recorded digest or size.",
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (
                    "BLOB_MISSING",
                    "A recorded blob is absent from canonical storage.",
                ),
                Err(_) => ("BLOB_UNREADABLE", "The recorded blob could not be read."),
            };
            report.issues.push(CheckIssue {
                severity: Severity::Error,
                code: failure.0.into(),
                objects: claim.objects.into_iter().collect(),
                reason: format!("{} ({digest})", failure.1),
                repair: None,
            });
        }
    }
}

fn invalid_artifact(report: &mut CheckReport, id: Id) {
    report.issues.push(CheckIssue {
        severity: Severity::Error,
        code: "ARTIFACT_METADATA_INVALID".into(),
        objects: vec![id],
        reason: "Retained artifact metadata could not identify its bytes.".into(),
        repair: None,
    });
}

pub(super) fn read_hash(mut input: impl Read) -> io::Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut chunk)?;
        if read == 0 {
            return Ok((format!("{:x}", hash.finalize()), size));
        }
        hash.update(&chunk[..read]);
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("blob too large"))?;
    }
}
