//! `exposure`: where exposed text lives in this workspace (plan §16.5).
//!
//! A diagnosis, never a remedy. It lists every local location that holds the
//! text of a record or event, or the bytes of a text read from a file: journal
//! events, retained blobs and their staged copies, the index, blob verdicts,
//! vault notes and their conflict copies, the render conflicts area, exports
//! and leftovers. It removes nothing, writes nothing and never prints the
//! searched text. The journal is append-only, so a later withdrawal,
//! retraction or classification event adds history and leaves the old bytes
//! in place; `investigation docs exposure` documents the manual remediation.
//!
//! The retained bytes of an Artifact whose effective classification hides
//! its content are not opened unless `--include-restricted` is passed, the
//! same rule every read path keeps. They are still listed, by path, as not
//! opened.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use investigation_core::{
    PROGRAM_NAME,
    classification::{AudiencePolicy, Classifier, Field},
    derivation::artifact_body,
    identity::{Id, Kind},
    state::State,
    wire::{ErrorCode, Failure},
};
use investigation_render::{
    Manifest,
    drift::is_conflict_copy,
    text::{display_text, inline_text},
    yaml,
};
use investigation_store::{
    Store,
    journal::CaseReplay,
    store::exposure::{self, Area, Form, MAX_TEXT_BYTES, MIN_TEXT_BYTES, event_path, verdict_path},
};
use serde_json::{Value, json};

use crate::{Global, Output};

pub(crate) const AFTER_HELP: &str = "\
Lists every local place that holds the exposed text: journal events, retained
blobs and staged copies, the index, blob verdicts, vault notes and conflict
copies, the render conflicts area, exports and leftovers. Give the record, case
or transaction ID whose text was exposed, the exposed text itself with
--text-file, or both. Pass the text in a file or on stdin, never as an argument,
so it stays out of shell history; it is never printed. A trailing line break is
ignored.

This is a diagnosis: it removes and changes nothing. The journal is append-only
and hash-chained, so a withdrawal, retraction or classification change leaves
the old bytes in place. Rotate the secret first, then follow the manual
remediation in investigation docs exposure.

Retained bytes of restricted Artifacts are listed but not opened unless you
pass --include-restricted.

Examples:
  investigation exposure obs_01a20751-9140-7d72-bbd2-181cd705230d --format json
  investigation exposure --text-file ./exposed.txt --format json
Next: investigation docs exposure";

const NOTICE: &str = "Diagnosis only: nothing was removed or changed. The journal is append-only and hash-chained, so withdrawal, retraction, redaction and classification events add history and leave the old bytes in place. Rotate the exposed secret first, then follow the manual remediation in investigation docs exposure. Copies outside this workspace cannot be recalled automatically.";

const LIMITATIONS: &[&str] = &[
    "Files are read, not devices: removed or overwritten files can survive in free blocks, filesystem snapshots, copy-on-write extents, device remapping and backups.",
    "SQLite can keep removed rows in free pages and its write-ahead log until the index file is replaced.",
    "A rendered note can hold the text cut short or reformatted beyond the searched forms; give the record ID to list the notes that project it.",
];

#[derive(clap::Args)]
#[command(group(
    clap::ArgGroup::new("subject")
        .args(["id", "text_file"])
        .required(true)
        .multiple(true)
))]
pub(crate) struct Args {
    /// Record, case or transaction ID (or command ID) whose text was exposed.
    #[arg(value_name = "ID")]
    id: Option<String>,
    /// Read the exposed text from FILE, or - for stdin. Never pass it as an argument.
    #[arg(long, value_name = "FILE|-")]
    text_file: Option<PathBuf>,
    /// Also open retained bytes of Artifacts classified above the default internal audience.
    #[arg(long)]
    include_restricted: bool,
}

/// How a location is known to hold the text, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Evidence {
    /// The searched bytes were found in the file or its name.
    Found,
    /// The record's own data is stored here.
    Holds,
    /// Derived from a location that holds the text; not confirmed by bytes.
    MayHold,
    /// Names the digest of bytes that hold the text; holds no text.
    DigestOnly,
}

impl Evidence {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Found => "found",
            Self::Holds => "holds",
            Self::MayHold => "may_hold",
            Self::DigestOnly => "digest_only",
        }
    }
}

struct Location {
    area: Area,
    evidence: Evidence,
    detail: String,
    forms: Vec<&'static str>,
    records: BTreeSet<Id>,
    not_opened: Option<String>,
}

#[derive(Default)]
struct Locations(BTreeMap<PathBuf, Location>);

impl Locations {
    fn add(
        &mut self,
        path: PathBuf,
        area: Area,
        evidence: Evidence,
        detail: impl Into<String>,
        records: impl IntoIterator<Item = Id>,
    ) -> &mut Location {
        let detail = detail.into();
        let entry = self.0.entry(path).or_insert_with(|| Location {
            area,
            evidence,
            detail: detail.clone(),
            forms: Vec::new(),
            records: BTreeSet::new(),
            not_opened: None,
        });
        if evidence < entry.evidence {
            entry.evidence = evidence;
            entry.detail = detail;
        }
        entry.records.extend(records);
        entry
    }
}

/// What the verified journal says about files: who they belong to.
#[derive(Default)]
struct Journal {
    /// Event file → the records it accepted.
    events: BTreeMap<PathBuf, BTreeSet<Id>>,
    /// Blob hex digest → every Artifact with a revision citing it.
    blobs: BTreeMap<String, BTreeSet<Id>>,
    /// Hex digests cited by an Artifact revision whose content the default
    /// audience may not see.
    restricted: BTreeSet<String>,
}

fn blob_hex(state: &State, record: &investigation_core::state::Record) -> Option<String> {
    let body = artifact_body(state, record.reference())?;
    let digest = body.digest?;
    Some(digest.as_str().strip_prefix("sha256:")?.to_owned())
}

impl Journal {
    fn build(cases: &BTreeMap<Id, CaseReplay>) -> Self {
        let mut journal = Self::default();
        let policy = AudiencePolicy::default();
        for (case_id, replay) in cases {
            for verified in &replay.events {
                let event = &verified.event;
                if let Some(path) = event_path(*case_id, event.sequence) {
                    journal
                        .events
                        .entry(path)
                        .or_default()
                        .extend(event.accepted.records.iter().map(|r| r.header.id));
                }
            }
            let classifier = Classifier::new(&replay.state);
            for current in replay.state.records_of_kind(Kind::Artifact) {
                for record in replay.state.history(current.header.id) {
                    let Some(hex) = blob_hex(&replay.state, record) else {
                        continue;
                    };
                    if !classifier.may_show(record, Field::Content, &policy) {
                        journal.restricted.insert(hex.clone());
                    }
                    journal
                        .blobs
                        .entry(hex)
                        .or_default()
                        .insert(record.header.id);
                }
            }
        }
        journal
    }
}

/// The rendered vault as the render record knows it.
#[derive(Default)]
struct Vault {
    /// Workspace-relative note path → the record it projects.
    notes: BTreeMap<PathBuf, Option<Id>>,
    /// Record → its workspace-relative note paths.
    by_record: BTreeMap<Id, Vec<PathBuf>>,
    /// Drift entries: owned note, withheld version, copies and moves.
    drift: Vec<(PathBuf, Option<PathBuf>, Vec<PathBuf>)>,
}

impl Vault {
    fn load(store: &Store) -> Self {
        let mut vault = Self::default();
        let Ok(loaded) = Manifest::read(store.workspace()) else {
            return vault;
        };
        let in_vault = |path: &str| Path::new("vault").join(path);
        for owned in loaded.manifest.owned() {
            let path = in_vault(owned.path.as_str());
            if let Some(id) = owned.projects {
                vault.by_record.entry(id).or_default().push(path.clone());
            }
            vault.notes.insert(path, owned.projects);
        }
        for drift in loaded.manifest.drift() {
            let mut copies: Vec<PathBuf> = drift.copies.iter().map(|p| in_vault(p)).collect();
            copies.extend(drift.renamed_to.iter().map(|p| in_vault(p)));
            vault.drift.push((
                in_vault(drift.path.as_str()),
                drift.conflict.as_ref().map(PathBuf::from),
                copies,
            ));
        }
        vault
    }

    /// Refine a generic vault path: an owned note, or a conflict copy beside one.
    fn area(&self, path: &Path) -> Area {
        if self.notes.contains_key(path) {
            return Area::VaultNote;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let beside = self.notes.keys().any(|owned| {
            owned.parent() == path.parent()
                && owned
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|owned| is_conflict_copy(name, owned))
        });
        if beside {
            Area::VaultConflictCopy
        } else {
            Area::Vault
        }
    }

    /// Every vault location of `id`'s generated notes, as recorded.
    fn of(&self, id: Id) -> Vec<(PathBuf, Area, &'static str)> {
        let mut out = Vec::new();
        for note in self.by_record.get(&id).into_iter().flatten() {
            out.push((
                note.clone(),
                Area::VaultNote,
                "generated note projecting the record; a restricted record renders as a stub",
            ));
            for (path, conflict, copies) in &self.drift {
                if path != note {
                    continue;
                }
                if let Some(conflict) = conflict {
                    out.push((
                        conflict.clone(),
                        Area::RenderConflict,
                        "generated version withheld from the drifted note",
                    ));
                }
                for copy in copies {
                    out.push((
                        copy.clone(),
                        Area::VaultConflictCopy,
                        "conflict copy or moved copy of the note",
                    ));
                }
            }
        }
        out
    }
}

fn read_text(path: &Path) -> Result<Vec<u8>, Failure> {
    let failure =
        |message: String| Failure::new(ErrorCode::InvalidInput, message).at("--text-file");
    let io_failure = |error: io::Error| {
        Failure::new(
            ErrorCode::IoFailure,
            format!("Read --text-file {}: {error}.", path.display()),
        )
        .at("--text-file")
    };
    let limit = MAX_TEXT_BYTES as u64 + 3;
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        io::stdin()
            .lock()
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(io_failure)?;
    } else {
        File::open(path)
            .map_err(io_failure)?
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(io_failure)?;
    }
    if bytes.ends_with(b"\n") {
        bytes.pop();
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
    }
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(failure(format!(
            "The exposed text is longer than {MAX_TEXT_BYTES} bytes. Search for a distinctive part of it, such as the secret alone."
        )));
    }
    if bytes.len() < MIN_TEXT_BYTES {
        return Err(failure(format!(
            "The exposed text has fewer than {MIN_TEXT_BYTES} bytes; text that short matches unrelated data."
        )));
    }
    Ok(bytes)
}

/// The text as stored raw and in each escaped form the tool writes it in.
fn forms(bytes: &[u8]) -> Vec<Form> {
    let mut forms = vec![Form {
        label: "raw",
        bytes: bytes.to_vec(),
    }];
    if let Ok(text) = std::str::from_utf8(bytes) {
        let json = serde_json::to_string(text).expect("a string serializes");
        let json = json[1..json.len() - 1].to_owned();
        let mut quoted = String::new();
        yaml::quoted(&mut quoted, text);
        let yaml = quoted
            .strip_prefix('"')
            .and_then(|q| q.strip_suffix('"'))
            .unwrap_or(&quoted)
            .to_owned();
        for (label, form) in [
            ("json", json),
            ("markdown", inline_text(text, usize::MAX)),
            ("title", display_text(text, usize::MAX).unwrap_or_default()),
            ("yaml", yaml),
        ] {
            if form.len() >= MIN_TEXT_BYTES && !forms.iter().any(|f| f.bytes == form.as_bytes()) {
                forms.push(Form {
                    label,
                    bytes: form.into_bytes(),
                });
            }
        }
    }
    forms
}

/// The subject records of `selector` and the case holding them.
fn resolve(
    cases: &BTreeMap<Id, CaseReplay>,
    selector: &str,
) -> Result<(Id, BTreeSet<Id>, Option<PathBuf>), Failure> {
    let parsed: Option<Id> = selector.parse().ok();
    // An event, by transaction ID or command ID.
    for (case_id, replay) in cases {
        for verified in &replay.events {
            let event = &verified.event;
            if Some(event.transaction_id) == parsed || event.command_id == selector {
                let records = event.accepted.records.iter().map(|r| r.header.id).collect();
                return Ok((*case_id, records, event_path(*case_id, event.sequence)));
            }
        }
    }
    if let Some(id) = parsed {
        for (case_id, replay) in cases {
            if replay.state.current(id).is_some() {
                return Ok((*case_id, expand(&replay.state, id), None));
            }
        }
    }
    Err(Failure::new(
        ErrorCode::UnknownReference,
        format!("No record, case, transaction or command ID {selector} is in this workspace's verified journal."),
    )
    .at("ID"))
}

/// The record, and for a Source or an Artifact every Artifact holding its
/// bytes or derived from them.
fn expand(state: &State, id: Id) -> BTreeSet<Id> {
    let mut subject = BTreeSet::from([id]);
    if id.kind() == Kind::Source {
        for artifact in state.records_of_kind(Kind::Artifact) {
            if artifact_body(state, artifact.reference()).is_some_and(|body| body.source.id == id) {
                subject.insert(artifact.header.id);
            }
        }
    }
    loop {
        let before = subject.len();
        for artifact in state.records_of_kind(Kind::Artifact) {
            let derived = artifact_body(state, artifact.reference()).is_some_and(|body| {
                body.transforms
                    .iter()
                    .any(|transform| subject.contains(&transform.input.id))
            });
            if derived {
                subject.insert(artifact.header.id);
            }
        }
        if subject.len() == before {
            return subject;
        }
    }
}

fn index_files(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("cache"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("index.sqlite"))
        .map(|name| Path::new("cache").join(name))
        .collect();
    files.sort();
    files
}

pub(crate) fn run(store: &mut Store, _global: &Global, args: Args) -> Result<Output, Failure> {
    let text = args.text_file.as_deref().map(read_text).transpose()?;
    let root = store.workspace().root().to_owned();
    let cases = store.cases();
    let mut limitations: Vec<String> = LIMITATIONS.iter().map(|s| (*s).to_owned()).collect();
    let (journal, cases) = match cases {
        Ok(cases) => (Some(Journal::build(&cases)), cases),
        Err(failure) => {
            limitations.insert(
                0,
                format!(
                    "The journal did not verify ({}), so files were not tied to records and no retained bytes were opened without --include-restricted.",
                    failure.error.message
                ),
            );
            (None, BTreeMap::new())
        }
    };
    let vault = Vault::load(store);
    let digest_files = exposure::digest_named_files(&root);
    let mut locations = Locations::default();

    let mut subject =
        json!({"id": Value::Null, "case_id": Value::Null, "records": [], "text": Value::Null});
    if let Some(selector) = args.id.as_deref() {
        if journal.is_none() {
            return Err(Failure::new(
                ErrorCode::IntegrityFailure,
                "The journal did not verify, so the ID cannot be traced. Run investigation check; give --text-file alone to search the files.",
            )
            .at("ID"));
        }
        let (case_id, records, event) = resolve(&cases, selector)?;
        subject["id"] = json!(selector);
        subject["case_id"] = json!(case_id);
        subject["records"] = json!(records);
        trace(
            &mut locations,
            &cases[&case_id],
            case_id,
            &records,
            event,
            &vault,
            &digest_files,
            &root,
        );
    }

    let mut scanned = Value::Null;
    let mut unopened = Vec::new();
    if let Some(bytes) = &text {
        let forms = forms(bytes);
        subject["text"] = json!({
            "bytes": bytes.len(),
            "forms": forms.iter().map(|f| f.label).collect::<Vec<_>>(),
        });
        let mut skip = BTreeMap::new();
        if !args.include_restricted {
            for (path, hex) in &digest_files {
                let reason = match &journal {
                    Some(journal) if journal.restricted.contains(hex) => {
                        "retained bytes of a restricted Artifact; pass --include-restricted to search them"
                    }
                    Some(_) => continue,
                    None => {
                        "retained bytes whose classification is unknown because the journal did not verify; pass --include-restricted to search them"
                    }
                };
                skip.insert(path.clone(), reason.to_owned());
            }
        }
        if let Some(input) = args.text_file.as_deref()
            && let (Ok(input), Ok(base)) = (input.canonicalize(), root.canonicalize())
            && let Ok(relative) = input.strip_prefix(&base)
        {
            skip.insert(
                relative.to_owned(),
                "the --text-file input itself".to_owned(),
            );
        }
        let report = exposure::scan(&root, &forms, &skip);
        scanned = json!({"files": report.files, "bytes": report.bytes});
        for hit in report.hits {
            let area = refine(&vault, &hit.path, hit.area);
            let records = owners(journal.as_ref(), &vault, &hit.path, area);
            let detail = if hit.forms.is_empty() {
                "the path name holds the text".to_owned()
            } else {
                format!("bytes match the {} form", hit.forms.join(", "))
            };
            let location = locations.add(hit.path, area, Evidence::Found, detail, records);
            location.forms = hit.forms;
        }
        derive(&mut locations, journal.as_ref(), &vault, &root);
        for item in report.unopened {
            let area = refine(&vault, &item.path, item.area);
            if let Some(location) = locations.0.get_mut(&item.path) {
                location.not_opened = Some(item.reason);
            } else {
                unopened.push(json!({
                    "path": display_path(&item.path, &forms),
                    "area": area.as_str(),
                    "reason": item.reason,
                    "records": owners(journal.as_ref(), &vault, &item.path, area),
                }));
            }
        }
    } else {
        limitations.push("Without --text-file only the record's own locations are listed; copies of its text quoted by other records, notes or files are not.".to_owned());
    }

    let forms = text.as_deref().map(forms).unwrap_or_default();
    let mut ordered: Vec<(&PathBuf, &Location)> = locations.0.iter().collect();
    ordered.sort_by(|a, b| (a.1.area, a.0).cmp(&(b.1.area, b.0)));
    let listed: Vec<Value> = ordered
        .iter()
        .map(|(path, location)| {
            json!({
                "path": display_path(path, &forms),
                "area": location.area.as_str(),
                "canonical": location.area.canonical(),
                "evidence": location.evidence.as_str(),
                "detail": location.detail,
                "forms": location.forms,
                "records": location.records,
                "not_opened": location.not_opened,
            })
        })
        .collect();
    let areas: BTreeMap<&str, &str> = ordered
        .iter()
        .map(|(_, location)| (location.area.as_str(), location.area.remediation()))
        .collect();
    let outside = outside(store);
    let next = json!([{
        "argv": [PROGRAM_NAME, "docs", "exposure"],
        "kind": "local_read",
        "effect": "Prints the manual remediation: rotate first, then restore from a pre-exposure copy or rebuild a sanitized fork.",
    }]);

    let mut lines = vec!["exposure  diagnosis only: nothing was removed or changed".to_owned()];
    if let Some(id) = subject["id"].as_str() {
        lines.push(format!(
            "subject  {id}  {} record(s) traced",
            subject["records"].as_array().map_or(0, Vec::len)
        ));
    }
    if let Some(text) = subject["text"].as_object() {
        lines.push(format!(
            "subject  text of {} bytes, searched as {}",
            text["bytes"],
            forms.iter().map(|f| f.label).collect::<Vec<_>>().join(", ")
        ));
    }
    for (path, location) in &ordered {
        let mut line = format!(
            "{:<11} {:<19} {}",
            location.evidence.as_str(),
            location.area.as_str(),
            display_path(path, &forms)
        );
        if !location.records.is_empty() {
            let ids: Vec<String> = location.records.iter().map(Id::to_string).collect();
            line.push_str(&format!("  [{}]", ids.join(", ")));
        }
        if let Some(reason) = &location.not_opened {
            line.push_str(&format!("  (not opened: {reason})"));
        }
        lines.push(line);
    }
    if listed.is_empty() {
        lines.push("no location in this workspace holds the text".to_owned());
    }
    for item in &unopened {
        lines.push(format!(
            "not_opened  {:<19} {}  ({})",
            item["area"].as_str().unwrap_or(""),
            item["path"].as_str().unwrap_or(""),
            item["reason"].as_str().unwrap_or("")
        ));
    }
    if let Some(scanned) = scanned.as_object() {
        lines.push(format!(
            "searched {} file(s), {} byte(s)",
            scanned["files"], scanned["bytes"]
        ));
    }
    for (area, remediation) in &areas {
        lines.push(format!("{area}: {remediation}"));
    }
    for item in outside.as_array().into_iter().flatten() {
        lines.push(format!(
            "outside  {}",
            item["what"].as_str().unwrap_or_default()
        ));
    }
    for limitation in &limitations {
        lines.push(format!("limitation  {limitation}"));
    }
    lines.push(NOTICE.to_owned());
    lines.push(format!("Next: {PROGRAM_NAME} docs exposure"));

    Ok(Output::success(
        "exposure_diagnosed",
        json!({
            "writes": "nothing",
            "subject": subject,
            "locations": listed,
            "not_opened": unopened,
            "searched": scanned,
            "areas": areas,
            "outside": outside,
            "limitations": limitations,
            "notice": NOTICE,
            "next": next,
        }),
        lines.join("\n"),
    ))
}

/// The record-based locations of `records` in `case_id`.
#[allow(clippy::too_many_arguments)]
fn trace(
    locations: &mut Locations,
    replay: &CaseReplay,
    case_id: Id,
    records: &BTreeSet<Id>,
    event: Option<PathBuf>,
    vault: &Vault,
    digest_files: &[(PathBuf, String)],
    root: &Path,
) {
    let state = &replay.state;
    if let Some(event) = event {
        locations.add(
            event,
            Area::JournalEvent,
            Evidence::Holds,
            "the event: its request and every record it accepted",
            records.iter().copied(),
        );
    }
    let mut hexes = BTreeMap::<String, BTreeSet<Id>>::new();
    for id in records {
        for record in state.history(*id) {
            if let Some(path) = event_path(case_id, record.header.acceptance_sequence) {
                locations.add(
                    path,
                    Area::JournalEvent,
                    Evidence::Holds,
                    format!(
                        "accepted revision {} of the record",
                        record.header.revision.0
                    ),
                    [*id],
                );
            }
            if let Some(hex) = blob_hex(state, record) {
                hexes.entry(hex).or_default().insert(*id);
            }
        }
        for (path, area, detail) in vault.of(*id) {
            if root.join(&path).is_file() {
                locations.add(path, area, Evidence::MayHold, detail, [*id]);
            }
        }
    }
    for (path, hex) in digest_files {
        if let Some(ids) = hexes.get(hex) {
            let area = exposure::area(path);
            let detail = if area == Area::Staging {
                "a staged copy of the record's retained bytes"
            } else {
                "the record's retained bytes"
            };
            locations.add(
                path.clone(),
                area,
                Evidence::Holds,
                detail,
                ids.iter().copied(),
            );
        }
    }
    for (hex, ids) in &hexes {
        let verdict = verdict_path(hex);
        if root.join(&verdict).is_file() {
            locations.add(
                verdict,
                Area::BlobVerdict,
                Evidence::DigestOnly,
                "names the digest of the record's retained bytes",
                ids.iter().copied(),
            );
        }
    }
    for path in index_files(root) {
        locations.add(
            path,
            Area::Index,
            Evidence::Holds,
            "projection rows and search text of the records",
            records.iter().copied(),
        );
    }
}

/// Locations that follow from a found one: the index and notes projecting
/// records of an event that holds the text, and verdicts and notes of blobs
/// that hold it.
fn derive(locations: &mut Locations, journal: Option<&Journal>, vault: &Vault, root: &Path) {
    let Some(journal) = journal else {
        return;
    };
    let found: Vec<(PathBuf, Area, BTreeSet<Id>)> = locations
        .0
        .iter()
        .filter(|(_, l)| l.evidence == Evidence::Found)
        .map(|(p, l)| (p.clone(), l.area, l.records.clone()))
        .collect();
    let mut in_journal = false;
    for (path, area, records) in found {
        match area {
            Area::JournalEvent => {
                in_journal = true;
                for id in &records {
                    for (note, area, _) in vault.of(*id) {
                        if root.join(&note).is_file() {
                            locations.add(
                                note,
                                area,
                                Evidence::MayHold,
                                "projects a record accepted in an event that holds the text",
                                [*id],
                            );
                        }
                    }
                }
            }
            Area::Blob | Area::Staging => {
                let Some(hex) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let verdict = verdict_path(hex);
                if root.join(&verdict).is_file() {
                    locations.add(
                        verdict,
                        Area::BlobVerdict,
                        Evidence::DigestOnly,
                        "names the digest of retained bytes that hold the text",
                        journal.blobs.get(hex).into_iter().flatten().copied(),
                    );
                }
            }
            _ => {}
        }
    }
    if in_journal {
        for path in index_files(root) {
            locations.add(
                path,
                Area::Index,
                Evidence::MayHold,
                "rebuilt from the journal, which holds the text",
                [],
            );
        }
    }
}

fn refine(vault: &Vault, path: &Path, area: Area) -> Area {
    if area == Area::Vault {
        vault.area(path)
    } else {
        area
    }
}

/// The records a file belongs to, as far as the journal and render record say.
fn owners(journal: Option<&Journal>, vault: &Vault, path: &Path, area: Area) -> BTreeSet<Id> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match (journal, area) {
        (Some(journal), Area::JournalEvent) => {
            journal.events.get(path).cloned().unwrap_or_default()
        }
        (Some(journal), Area::Blob | Area::Staging | Area::BlobVerdict) => {
            journal.blobs.get(name).cloned().unwrap_or_default()
        }
        (_, Area::VaultNote) => vault
            .notes
            .get(path)
            .copied()
            .flatten()
            .into_iter()
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// A workspace-relative path for output, with any form of the text in it
/// replaced, so the report never repeats the text.
fn display_path(path: &Path, forms: &[Form]) -> String {
    let mut shown = path.to_string_lossy().into_owned();
    for form in forms {
        if let Ok(text) = std::str::from_utf8(&form.bytes) {
            shown = shown.replace(text, "[exposed text]");
        }
    }
    shown
}

/// Places beyond this workspace the tool knows of, which it never searches.
fn outside(store: &Store) -> Value {
    let mut items: Vec<Value> = store
        .workspace()
        .config()
        .import_roots
        .iter()
        .map(|root| {
            json!({
                "kind": "import_root",
                "path": root,
                "what": format!("import root {}: the original files imported from it may hold the text; not searched", root.display()),
            })
        })
        .collect();
    items.push(json!({
        "kind": "distributed_copies",
        "path": Value::Null,
        "what": "copies made outside this workspace (synced or shared vaults, backups, exports sent on, other clones, terminal and shell history, logs) cannot be recalled automatically; not searched",
    }));
    Value::Array(items)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::CommandFactory;

    use super::*;

    /// Words that would claim an erasure the tool never performs. Built at
    /// run time so this file does not match itself.
    fn claims() -> [String; 2] {
        [["pur", "ged"].concat(), ["securely", " deleted"].concat()]
    }

    fn offending(text: &str) -> Option<String> {
        let lower = text.to_lowercase();
        claims().into_iter().find(|claim| lower.contains(claim))
    }

    fn help_texts(mut command: clap::Command, path: String, out: &mut Vec<(String, String)>) {
        out.push((path.clone(), command.render_long_help().to_string()));
        for sub in command.get_subcommands() {
            help_texts(sub.clone(), format!("{path} {}", sub.get_name()), out);
        }
    }

    fn files_under(dir: &Path, extensions: &[&str], out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_under(&path, extensions, out);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.contains(&e))
            {
                out.push(path);
            }
        }
    }

    #[test]
    fn no_command_or_doc_calls_withdrawal_or_redaction_an_erasure() {
        let mut helps = Vec::new();
        help_texts(
            crate::shortcuts::augment(crate::Cli::command()),
            PROGRAM_NAME.to_owned(),
            &mut helps,
        );
        assert!(helps.iter().any(|(path, _)| path.ends_with(" exposure")));
        for (path, help) in &helps {
            assert_eq!(offending(help), None, "`{path} --help`");
        }
        for page in crate::docs::PAGES {
            assert_eq!(offending(page.body), None, "docs {}", page.name);
        }
        for text in [AFTER_HELP, NOTICE]
            .into_iter()
            .chain(LIMITATIONS.iter().copied())
        {
            assert_eq!(offending(text), None, "{text}");
        }
        // Shipped prose and every message compiled into the binaries, plus
        // the rendered golden vaults.
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = vec![repository.join("README.md")];
        files_under(&repository.join("docs"), &["md"], &mut files);
        files_under(&repository.join("crates"), &["rs"], &mut files);
        files_under(&repository.join("methods"), &["toml", "md"], &mut files);
        files_under(
            &repository.join("fixtures/vault-goldens"),
            &["md"],
            &mut files,
        );
        assert!(files.len() > 50);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            assert_eq!(offending(&text), None, "{}", file.display());
        }
    }

    #[test]
    fn forms_cover_the_escapes_the_tool_writes_and_skip_duplicates() {
        let plain = forms(b"plainsecret42");
        assert_eq!(plain.len(), 1);
        let labels: Vec<_> = forms(b"tok_en.\"x").iter().map(|f| f.label).collect();
        assert!(
            labels.contains(&"raw") && labels.contains(&"json") && labels.contains(&"markdown")
        );
        assert_eq!(forms(&[0xff, 0xfe, 0x00, 0x01]).len(), 1);
    }
}
