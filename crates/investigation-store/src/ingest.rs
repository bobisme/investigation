use std::io::Read;

use investigation_core::{
    case::{CaseCreate, CaseMode},
    evidence::{
        AcquisitionChannel, ArtifactAvailability, ArtifactBody, ArtifactIntegrity,
        ArtifactTransform, SourceAccess, SourceAcquisition, SourceBody,
    },
    header::{
        ActorDescriptor, Classification, ClassificationLevel, ClockQuality, Knowledge,
        RecordHeader, Revision, SchemaVersion, SourceTime, UtcTimestamp,
    },
    input,
    state::Record,
    wire::{Digest, RecordChange},
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::Deserialize;

use super::*;
use crate::{blob::stage, import_paths::ImportRoot};

pub const MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ITEMS: usize = 32;
pub const TEXT_PARSER_VERSION: &str = "inv.text.utf8.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum IngestVersion {
    #[serde(rename = "inv.ingest.v1", alias = "inv.ingest.v0-alpha")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: IngestVersion,
    #[schemars(length(min = 1, max = 128))]
    pub command_id: String,
    pub actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_descriptor: Option<ActorDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_case: Option<CaseCreate>,
    pub source: SourceManifest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<StreamContent>,
    #[serde(default)]
    pub extract_text: bool,
    #[serde(default)]
    pub classification: ClassificationLevel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    pub title: String,
    pub producer: Option<String>,
    pub publisher: Option<String>,
    pub identity: Option<String>,
    pub format: Option<String>,
    pub context: Option<String>,
    #[serde(default)]
    pub verification_limits: Vec<String>,
    /// Opaque producer metadata retained with the resulting Source.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamContent {
    Stdin { name: String },
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ItemReceipt {
    pub source_id: Id,
    pub artifact_id: Id,
    pub digest: Digest,
    pub size_bytes: u64,
    pub derivative_id: Option<Id>,
    pub text_status: TextStatus,
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextStatus {
    NotRequested,
    Decoded,
    InvalidUtf8,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IngestReceipt {
    #[serde(flatten)]
    pub receipt: Receipt,
    pub items: Vec<ItemReceipt>,
    pub manifest_artifact_id: Option<Id>,
}

struct PreparedItem {
    label: String,
    digest: Digest,
    size: u64,
    channel: AcquisitionChannel,
    text: TextStatus,
}

pub fn manifest_schema() -> Schema {
    schema_for!(Manifest)
}

pub fn receipt_schema() -> Schema {
    schema_for!(IngestReceipt)
}

pub fn parse_manifest(bytes: &[u8]) -> Result<(Manifest, Value), Failure> {
    let raw = input::parse_request(bytes).map_err(|error| invalid(error.message, &error.path))?;
    let manifest: Manifest =
        input::decode(&raw).map_err(|error| invalid(error.message, &error.path))?;
    if manifest.command_id.is_empty()
        || manifest.command_id.chars().count() > 128
        || manifest.command_id.chars().any(char::is_control)
    {
        return Err(invalid(
            "Provide a command ID with 1–128 characters.",
            "/command_id",
        ));
    }
    match (&manifest.new_case, manifest.case_id) {
        (Some(case), None)
            if case.mode == CaseMode::Explore
                && !case.title.trim().is_empty()
                && case.title.chars().count() <= 256 => {}
        (None, Some(id)) if id.kind() == Kind::Case => {}
        _ => {
            return Err(invalid(
                "Choose one existing case_id or an explore new_case with a title.",
                "/case_id",
            ));
        }
    }
    if manifest.paths.is_empty() == manifest.content.is_none() || manifest.paths.len() > MAX_ITEMS {
        return Err(invalid(
            "Supply a bounded nonempty paths array or one stdin content descriptor.",
            "/paths",
        ));
    }
    if manifest.source.title.trim().is_empty() || manifest.source.title.chars().count() > 256 {
        return Err(invalid(
            "Provide a source title of 1–256 characters.",
            "/source/title",
        ));
    }
    Ok((manifest, raw))
}

impl Store {
    /// Snapshot local inputs before acquiring the canonical commit lock.
    pub fn ingest(
        &mut self,
        bytes: &[u8],
        stream: Option<&mut dyn Read>,
    ) -> Result<IngestReceipt, Failure> {
        let (manifest, raw) = parse_manifest(bytes)?;
        let mut stages = Vec::new();
        let result = self.ingest_inner(bytes, manifest, raw, stream, &mut stages);
        if result
            .as_ref()
            .is_err_and(|error| error.error.code == ErrorCode::DurabilityUnknown)
        {
            self.note_stopped();
        }
        for staged in &stages {
            let _ = self.fs.remove(&staged.temp);
        }
        result
    }

    fn ingest_inner(
        &mut self,
        bytes: &[u8],
        manifest: Manifest,
        raw: Value,
        stream: Option<&mut dyn Read>,
        stages: &mut Vec<StagedBlob>,
    ) -> Result<IngestReceipt, Failure> {
        let mut prepared = Vec::new();
        for (index, path) in manifest.paths.iter().enumerate() {
            let mut opened = self.open_import(path, index)?;
            let staged = stage_input(&mut self.fs, &self.gate, Limited::new(&mut opened.file))?;
            stages.push(staged);
            if !opened
                .witness
                .verify_unchanged(&opened.file)
                .map_err(io_failure)?
            {
                return Err(Failure::new(
                    ErrorCode::IoFailure,
                    "The source changed while it was read. Retry with a stable snapshot.",
                )
                .at(format!("/paths/{index}")));
            }
            prepared.push(self.prepare_item(
                stages.last().expect("just staged"),
                path.to_string_lossy().into_owned(),
                AcquisitionChannel::LocalFile,
                manifest.extract_text,
            )?);
        }
        if let Some(StreamContent::Stdin { name }) = &manifest.content {
            if name.trim().is_empty() || name.chars().count() > 256 {
                return Err(invalid(
                    "Name the stdin source with 1–256 characters.",
                    "/content/name",
                ));
            }
            let stream = stream.ok_or_else(|| {
                invalid(
                    "Supply original bytes on stdin using a manifest file.",
                    "/content",
                )
            })?;
            stages.push(stage_input(&mut self.fs, &self.gate, Limited::new(stream))?);
            prepared.push(self.prepare_item(
                stages.last().expect("just staged"),
                name.clone(),
                AcquisitionChannel::Producer,
                manifest.extract_text,
            )?);
        }
        let attachments = prepared.iter().map(|item| item.digest.clone()).collect();
        let staged_manifest = stage_input(&mut self.fs, &self.gate, bytes)?;
        let manifest_digest = digest_for(&staged_manifest)?;
        let manifest_size = staged_manifest.size;
        stages.push(staged_manifest);
        let receipt = self.commit_plan(
            CommitPlan {
                case_id: manifest.case_id,
                event: EventInput {
                    actor: &manifest.actor,
                    command_id: &manifest.command_id,
                    preconditions: &[],
                    manifest_digest: Some(manifest_digest.clone()),
                },
                request: SemanticRequest {
                    input: raw,
                    attachments,
                },
                blobs: stages,
            },
            &mut SystemClock,
            &mut SystemIdSource,
            |_, _, acceptance| {
                create_records(
                    &manifest,
                    &prepared,
                    &manifest_digest,
                    manifest_size,
                    acceptance,
                )
            },
        )?;
        Ok(ingest_receipt(receipt, prepared))
    }

    fn open_import(
        &self,
        path: &Path,
        index: usize,
    ) -> Result<crate::import_paths::OpenedImport, Failure> {
        let mut failure = None;
        for configured in &self.workspace.config().import_roots {
            let root = if configured.is_absolute() {
                configured.clone()
            } else {
                self.workspace.root().join(configured)
            };
            let relative = if path.is_absolute() {
                let Ok(relative) = path.strip_prefix(&root) else {
                    continue;
                };
                relative
            } else {
                path
            };
            match ImportRoot::open(&root).and_then(|dir| dir.open_file(relative, MAX_IMPORT_BYTES))
            {
                Ok(opened) => return Ok(opened),
                Err(error) => failure = Some(error.to_string()),
            }
        }
        Err(Failure::new(
            ErrorCode::InvalidInput,
            failure.unwrap_or_else(|| {
                format!(
                    "Path {} is outside configured import roots.",
                    path.display()
                )
            }),
        )
        .at(format!("/paths/{index}"))
        .with_schema("ingest"))
    }

    fn prepare_item(
        &self,
        staged: &StagedBlob,
        label: String,
        channel: AcquisitionChannel,
        extract: bool,
    ) -> Result<PreparedItem, Failure> {
        let text = if extract {
            let mut data = Vec::new();
            self.fs
                .read(&staged.temp)
                .map_err(io_failure)?
                .take(MAX_IMPORT_BYTES + 1)
                .read_to_end(&mut data)
                .map_err(io_failure)?;
            if std::str::from_utf8(&data).is_ok() {
                TextStatus::Decoded
            } else {
                TextStatus::InvalidUtf8
            }
        } else {
            TextStatus::NotRequested
        };
        Ok(PreparedItem {
            label,
            digest: digest_for(staged)?,
            size: staged.size,
            channel,
            text,
        })
    }
}

fn create_records(
    manifest: &Manifest,
    items: &[PreparedItem],
    manifest_digest: &Digest,
    manifest_size: u64,
    acceptance: Acceptance<'_>,
) -> Result<AcceptedTransaction, Failure> {
    let case_id = match manifest.case_id {
        Some(case_id) => case_id,
        None => new_id(acceptance.ids, Kind::Case)?,
    };
    let mut builder = IngestRecords {
        manifest,
        case_id,
        timestamp: acceptance.clock.now_utc(),
        sequence: acceptance.sequence,
        ids: acceptance.ids,
        records: Vec::new(),
        created: Vec::new(),
    };
    if let Some(create) = &manifest.new_case {
        builder.add(
            case_id,
            "case".into(),
            create.clone().into_body(),
            Vec::new(),
        )?;
    }
    let acquired_at = Some(SourceTime {
        timestamp: Knowledge::Known(builder.timestamp.to_string()),
        offset: Knowledge::Known("+00:00".into()),
        clock_quality: ClockQuality::Unverified,
    });
    let mut first_source = None;
    for (index, item) in items.iter().enumerate() {
        let source_id = new_id(builder.ids, Kind::Source)?;
        let source_ref = RevisionRef {
            id: source_id,
            revision: 1,
        };
        let source = SourceBody {
            producer: manifest.source.producer.clone(),
            publisher: manifest.source.publisher.clone(),
            identity: manifest
                .source
                .identity
                .clone()
                .or_else(|| Some(format!("{} / {}", manifest.source.title, item.label))),
            acquisition: SourceAcquisition {
                channel: item.channel,
                acquired_at: acquired_at.clone(),
                method: Some("local byte snapshot".into()),
                context: manifest.source.context.clone(),
                environment: None,
            },
            access: if manifest.classification == ClassificationLevel::Restricted {
                SourceAccess::Restricted
            } else {
                SourceAccess::LocalOnly
            },
            verification_limits: manifest.source.verification_limits.clone(),
            metadata: manifest.source.metadata.clone(),
        };
        first_source.get_or_insert(source_ref);
        builder.add(source_id, format!("source:{index}"), source, Vec::new())?;

        let artifact_id = new_id(builder.ids, Kind::Artifact)?;
        let original = ArtifactBody {
            digest: Some(item.digest.clone()),
            size_bytes: Some(item.size),
            format: manifest.source.format.clone(),
            source: source_ref,
            transforms: Vec::new(),
            availability: ArtifactAvailability::Retained,
            integrity: ArtifactIntegrity::Verified,
            sensitivity: sensitivity(manifest.classification),
        };
        builder.add(
            artifact_id,
            format!("artifact:{index}"),
            original.clone(),
            vec![source_id],
        )?;
        if matches!(item.text, TextStatus::Decoded) {
            // Strict UTF-8 decoding with unchanged newline and character bytes.
            // The derivative gets its own record even when its blob deduplicates.
            let text_id = new_id(builder.ids, Kind::Artifact)?;
            let derivative = ArtifactBody {
                format: Some("text/plain; charset=utf-8".into()),
                transforms: vec![ArtifactTransform {
                    input: RevisionRef {
                        id: artifact_id,
                        revision: 1,
                    },
                    transformation: "decode_utf8".into(),
                    version: TEXT_PARSER_VERSION.into(),
                }],
                ..original
            };
            builder.add(
                text_id,
                format!("text:{index}"),
                derivative,
                vec![source_id, artifact_id],
            )?;
        }
    }
    let source = first_source.ok_or_else(|| integrity("Ingest has no source acquisition."))?;
    let id = new_id(builder.ids, Kind::Artifact)?;
    builder.add(
        id,
        "manifest".into(),
        ArtifactBody {
            digest: Some(manifest_digest.clone()),
            size_bytes: Some(manifest_size),
            format: Some("application/json".into()),
            source,
            transforms: Vec::new(),
            availability: ArtifactAvailability::Retained,
            integrity: ArtifactIntegrity::Verified,
            sensitivity: sensitivity(manifest.classification),
        },
        vec![source.id],
    )?;
    Ok(AcceptedTransaction {
        case_id,
        sequence: builder.sequence,
        records: builder.records,
        created: builder.created,
        updated: Vec::new(),
        warnings: Vec::new(),
    })
}

struct IngestRecords<'a> {
    manifest: &'a Manifest,
    case_id: Id,
    timestamp: UtcTimestamp,
    sequence: u64,
    ids: &'a mut dyn IdSource,
    records: Vec<Record>,
    created: Vec<RecordChange>,
}

impl IngestRecords<'_> {
    fn add(
        &mut self,
        id: Id,
        client_ref: String,
        body: impl Serialize,
        refs: Vec<Id>,
    ) -> Result<(), Failure> {
        let actor = self.manifest.actor.clone();
        let kind = id.kind();
        let level = if kind == Kind::Case {
            ClassificationLevel::Internal
        } else {
            self.manifest.classification
        };
        let acquired_at = matches!(kind, Kind::Source | Kind::Artifact).then(|| SourceTime {
            timestamp: Knowledge::Known(self.timestamp.to_string()),
            offset: Knowledge::Known("+00:00".into()),
            clock_quality: ClockQuality::Unverified,
        });
        self.records.push(Record {
            header: RecordHeader {
                id,
                case_id: self.case_id,
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: actor.clone(),
                actor_descriptor_self_reported: self.manifest.actor_descriptor.clone(),
                created_at: self.timestamp.clone(),
                recorded_at: self.timestamp.clone(),
                recorded_by: actor,
                observed_at: None,
                acquired_at,
                acceptance_sequence: self.sequence,
                revision: Revision(1),
                history: Vec::new(),
                classification: Classification {
                    level,
                    ..Classification::default()
                },
                provenance: investigation_core::header::Provenance { references: refs },
                extensions: BTreeMap::new(),
            },
            body: serde_json::to_value(body)
                .map_err(|_| integrity("Could not encode the retained source record."))?,
        });
        self.created.push(RecordChange {
            id,
            revision: 1,
            client_ref: Some(client_ref),
        });
        Ok(())
    }
}

fn sensitivity(level: ClassificationLevel) -> investigation_core::evidence::ArtifactSensitivity {
    use investigation_core::evidence::ArtifactSensitivity;
    match level {
        ClassificationLevel::Public => ArtifactSensitivity::Public,
        ClassificationLevel::Internal => ArtifactSensitivity::Internal,
        ClassificationLevel::Restricted => ArtifactSensitivity::Restricted,
    }
}

fn ingest_receipt(mut receipt: Receipt, items: Vec<PreparedItem>) -> IngestReceipt {
    let id = |client_ref: &str| {
        receipt.created.iter().find_map(|change| {
            (change.client_ref.as_deref() == Some(client_ref)).then_some(change.id)
        })
    };
    let manifest_artifact_id = id("manifest");
    let expected_count = items.len();
    let items = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            Some(ItemReceipt {
                source_id: id(&format!("source:{index}"))?,
                artifact_id: id(&format!("artifact:{index}"))?,
                digest: item.digest,
                size_bytes: item.size,
                derivative_id: id(&format!("text:{index}")),
                text_status: item.text,
            })
        })
        .collect::<Vec<_>>();
    if manifest_artifact_id.is_none() || items.len() != expected_count {
        receipt.warnings.push("The canonical receipt is available. Some import item details could not be reconstructed.".into());
    }
    IngestReceipt {
        receipt,
        items,
        manifest_artifact_id,
    }
}

fn new_id(ids: &mut dyn IdSource, kind: Kind) -> Result<Id, Failure> {
    ids.next_id(kind)
        .map_err(|_| integrity("Could not allocate an import record ID."))
}

fn digest_for(staged: &StagedBlob) -> Result<Digest, Failure> {
    format!("sha256:{}", staged.digest)
        .parse()
        .map_err(|_| integrity("Staged blob has an invalid digest."))
}

fn stage_input(
    fs: &mut impl DurableFs,
    gate: &WriteGate,
    input: impl Read,
) -> Result<StagedBlob, Failure> {
    let id = new_id(&mut SystemIdSource, Kind::Import)?;
    let path = Path::new(TMP).join(format!("import-{id}.tmp"));
    stage(fs, gate, &path, input).map_err(blob_failure)
}

fn invalid(message: &str, path: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message)
        .at(path)
        .with_schema("ingest")
}

struct Limited<R> {
    input: R,
    remaining: u64,
}

impl<R> Limited<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            remaining: MAX_IMPORT_BYTES,
        }
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 && !buffer.is_empty() {
            if self.input.read(&mut [0; 1])? == 0 {
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "Local import exceeds 64 MiB.",
            ));
        }
        let limit = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = self.input.read(&mut buffer[..limit])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}
