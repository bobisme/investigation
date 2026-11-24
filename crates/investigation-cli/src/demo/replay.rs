//! The replay engine of `demo`: a fixture workspace's committed
//! transactions submitted again, in commit order, through the same store
//! entry points `record` and `ingest` use, with every fixture identity
//! rewritten to the one the replay produced at the same place.
//!
//! It names nothing of the CLI, so a test can replay several fixtures into
//! one workspace through the same code (`tests/clean_restore.rs` includes
//! this file).

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    ops::Range,
    path::{Component, Path, PathBuf},
};

use investigation_core::{
    basis::basis_digest,
    header::{Actor, UtcTimestamp},
    identity::{Id, Kind},
    replay_seams::Replay,
    state::Record,
    wire::{ErrorCode, Failure, Receipt},
};
use investigation_store::{
    Store, journal::hash_bytes, store::ingest::producer_run::ProducerRunSubmission,
};
use serde_json::Value;

/// What one replay committed.
pub(crate) struct Replayed {
    /// Transactions submitted.
    pub(crate) transactions: usize,
    /// The case of the first transaction.
    pub(crate) first_case: Option<Id>,
}

/// Submit `fixture`'s transactions to `store` in commit order, holding
/// `clock` at each one's recorded time, until the first one `stop` accepts.
/// The fixture's workspace ID maps to the store's. A request the current
/// core rejects stops the replay; the error names the transaction.
pub(crate) fn replay<'a>(
    store: &mut Store,
    clock: &Replay,
    fixture: &Fixture<'a>,
    stop: impl Fn(&Event<'a>) -> bool,
) -> Result<Replayed, String> {
    let root = store.workspace().root().to_path_buf();
    let mut map = Mapping::default();
    map.learn(&fixture.workspace_id, &store.workspace().id().to_string())?;
    let mut replayed = Replayed {
        transactions: 0,
        first_case: None,
    };
    for event in fixture.events.iter().take_while(|event| !stop(event)) {
        clock.set_now(event.recorded_at.clone());
        let stopped = |message: String| {
            format!(
                "Replay stopped at transaction {} ({}): {message}",
                replayed.transactions + 1,
                event.command_id
            )
        };
        let input = map.rewrite(&event.value["request"]["input"]);
        let (receipt, case) = submit(store, &root, fixture, event, &input)
            .map_err(|failure| stopped(failure.error.message))?;
        let case = match map.get(&event.case_id) {
            Some(case) => case
                .parse::<Id>()
                .map_err(|error| stopped(error.to_string()))?,
            None => case
                .or_else(|| {
                    receipt
                        .created
                        .iter()
                        .map(|change| change.id)
                        .find(|id| id.kind() == Kind::Case)
                })
                .ok_or_else(|| stopped("no case for the transaction".into()))?,
        };
        replayed.first_case.get_or_insert(case);
        let path = root.join(format!(
            "state/cases/{case}/events/{:012}.json",
            receipt.snapshot.sequence
        ));
        let bytes = fs::read(&path).map_err(|error| stopped(error.to_string()))?;
        let committed: Value =
            serde_json::from_slice(&bytes).map_err(|error| stopped(error.to_string()))?;
        map.learn_event(event, &committed, &bytes)
            .map_err(stopped)?;
        replayed.transactions += 1;
    }
    Ok(replayed)
}

/// One committed transaction of the fixture.
pub(crate) struct Event<'a> {
    value: Value,
    bytes: &'a [u8],
    pub(crate) recorded_at: UtcTimestamp,
    case_id: String,
    sequence: u64,
    pub(crate) command_id: String,
}

impl Event<'_> {
    /// Whether the transaction closes a case.
    pub(crate) fn closes_case(&self) -> bool {
        self.value["request"]["input"]["operations"]
            .as_array()
            .is_some_and(|operations| {
                operations
                    .iter()
                    .any(|operation| operation["op"] == "case.close")
            })
    }
}

/// A fixture workspace's files, read as the transactions to replay.
pub(crate) struct Fixture<'a> {
    pub(crate) config: &'a str,
    pub(crate) workspace_id: String,
    pub(crate) events: Vec<Event<'a>>,
    blobs: HashMap<String, &'a [u8]>,
}

impl<'a> Fixture<'a> {
    /// Read a fixture from its files, by workspace-relative path: the
    /// config, the workspace marker, case events and retained blobs; other
    /// files are ignored. A broken fixture is described in the error.
    pub(crate) fn load(files: &[(&str, &'a [u8])]) -> Result<Self, String> {
        let broken = |message: &str| message.to_owned();
        let mut config = None;
        let mut workspace_id = None;
        let mut events = Vec::new();
        let mut blobs = HashMap::new();
        for &(path, bytes) in files {
            if path == "investigation.toml" {
                config = std::str::from_utf8(bytes).ok();
            } else if path == "state/workspace.json" {
                let marker: Value = serde_json::from_slice(bytes)
                    .map_err(|_| broken("Its workspace marker is not JSON."))?;
                workspace_id = marker["workspace_id"].as_str().map(str::to_owned);
            } else if let Some(name) = path.strip_prefix("state/blobs/sha256/") {
                let hex = name.rsplit('/').next().unwrap_or_default();
                blobs.insert(format!("sha256:{hex}"), bytes);
            } else if path.contains("/events/") {
                let value: Value = serde_json::from_slice(bytes)
                    .map_err(|_| broken(&format!("{path} is not JSON.")))?;
                let case_id = path.split('/').nth(2).unwrap_or_default().to_owned();
                let recorded_at = value["recorded_at"]
                    .as_str()
                    .and_then(|text| text.parse().ok())
                    .ok_or_else(|| broken(&format!("{path} has no recorded_at.")))?;
                events.push(Event {
                    sequence: value["sequence"].as_u64().unwrap_or_default(),
                    command_id: value["command_id"].as_str().unwrap_or_default().to_owned(),
                    value,
                    bytes,
                    recorded_at,
                    case_id,
                });
            }
        }
        // Commit order across cases: recorded time, then the case's own order.
        events.sort_by(|a, b| {
            (a.recorded_at.to_string(), &a.case_id, a.sequence).cmp(&(
                b.recorded_at.to_string(),
                &b.case_id,
                b.sequence,
            ))
        });
        Ok(Self {
            config: config.ok_or_else(|| broken("It has no investigation.toml."))?,
            workspace_id: workspace_id.ok_or_else(|| broken("It has no workspace marker."))?,
            events,
            blobs,
        })
    }

    fn blob(&self, digest: &str) -> Result<&'a [u8], Failure> {
        self.blobs.get(digest).copied().ok_or_else(|| {
            Failure::new(
                ErrorCode::IntegrityFailure,
                format!("The fixture retains no blob {digest}."),
            )
        })
    }
}

/// Submit one rewritten request through the entry point its command used.
/// Returns the receipt and, for a producer run, the case it names.
fn submit(
    store: &mut Store,
    root: &Path,
    fixture: &Fixture<'_>,
    event: &Event<'_>,
    input: &Value,
) -> Result<(Receipt, Option<Id>), Failure> {
    if input["schema_version"] == "inv.command.v1" {
        let bytes = serde_json::to_vec(input).expect("JSON serializes");
        return store.record(&bytes).map(|receipt| (receipt, None));
    }
    let mut staged = Staged::new(root);
    let attachments: Vec<&str> = event.value["request"]["attachments"]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(envelope) = input.get("producer_run") {
        // The declared artifacts are read from the workspace, as the
        // scenario proof placed them; only retained ones can be replayed.
        for artifact in envelope["artifacts"].as_array().into_iter().flatten() {
            let (Some(path), Some(hex)) = (
                artifact["path"].as_str(),
                artifact["expected_sha256"].as_str(),
            ) else {
                continue;
            };
            let digest = format!("sha256:{hex}");
            if attachments.contains(&digest.as_str()) {
                staged.write(path, fixture.blob(&digest)?)?;
            }
        }
        let actor: Actor = serde_json::from_value(input["actor"].clone())
            .map_err(|error| invalid(&error.to_string()))?;
        let mut submission = ProducerRunSubmission::new(actor);
        submission.case_id = input["case_id"]
            .as_str()
            .map(str::parse)
            .transpose()
            .map_err(|_| invalid("The producer run names no valid case."))?;
        let bytes = serde_json::to_vec(envelope).expect("JSON serializes");
        let receipt = store.ingest_producer_run(&bytes, &submission)?;
        return Ok((receipt.receipt, Some(receipt.case_id)));
    }
    if input.get("content").is_some() {
        return Err(invalid(
            "Replaying a standard-input ingest is not supported.",
        ));
    }
    // Retained paths, in order, are the transaction's attachments.
    let paths = input["paths"].as_array().cloned().unwrap_or_default();
    if input["retention"] != "reference" {
        for (path, digest) in paths.iter().zip(&attachments) {
            let path = path
                .as_str()
                .ok_or_else(|| invalid("An ingest path is not text."))?;
            staged.write(path, fixture.blob(digest)?)?;
        }
    }
    let bytes = serde_json::to_vec(input).expect("JSON serializes");
    store
        .ingest(&bytes, None)
        .map(|receipt| (receipt.receipt, None))
}

fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message.to_owned())
}

/// Source files placed in the new workspace for one ingest and removed
/// afterwards, whatever its outcome.
struct Staged<'a> {
    root: &'a Path,
    files: Vec<PathBuf>,
}

impl<'a> Staged<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: Vec::new(),
        }
    }

    fn write(&mut self, relative: &str, bytes: &[u8]) -> Result<(), Failure> {
        let relative = Path::new(relative);
        let plain = relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
            && relative.components().count() > 0;
        let mut components = relative.components().filter(|c| *c != Component::CurDir);
        let single = components.next().is_some() && components.next().is_none();
        if !plain || !single {
            return Err(invalid(&format!(
                "Fixture path {} is not a file name in the workspace.",
                relative.display()
            )));
        }
        let path = self.root.join(relative);
        let io = |error: std::io::Error| {
            Failure::new(
                ErrorCode::IoFailure,
                format!("Stage {}: {error}", path.display()),
            )
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io)?;
        self.files.push(path.clone());
        std::io::Write::write_all(&mut file, bytes).map_err(io)
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = fs::remove_file(file);
        }
    }
}

/// Fixture identity to replayed identity: record IDs, journal hashes and
/// digests, learned from each committed transaction.
#[derive(Default)]
pub(crate) struct Mapping(HashMap<String, String>);

impl Mapping {
    fn get(&self, token: &str) -> Option<&String> {
        self.0.get(token)
    }

    pub(crate) fn learn(&mut self, fixture: &str, replayed: &str) -> Result<(), String> {
        if fixture == replayed {
            return Ok(());
        }
        match self.0.get(fixture) {
            Some(known) if known != replayed => Err(format!(
                "the fixture's {fixture} replayed as both {known} and {replayed}"
            )),
            _ => {
                self.0.insert(fixture.to_owned(), replayed.to_owned());
                Ok(())
            }
        }
    }

    /// Learn from one committed transaction: every token at the same place
    /// in the two events, the event's own hash, and the review basis digest
    /// of every record it accepted.
    fn learn_event(
        &mut self,
        event: &Event<'_>,
        committed: &Value,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.align(&event.value, committed)?;
        self.learn(hash_bytes(event.bytes).as_str(), hash_bytes(bytes).as_str())?;
        let records = |value: &Value| {
            value["accepted_operations"]["records"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        for (fixture, replayed) in records(&event.value).iter().zip(records(committed).iter()) {
            let digest = |value: &Value| {
                serde_json::from_value::<Record>(value.clone())
                    .ok()
                    .and_then(|record| basis_digest(&record).ok())
            };
            if let (Some(fixture), Some(replayed)) = (digest(fixture), digest(replayed)) {
                self.learn(fixture.as_str(), replayed.as_str())?;
            }
        }
        Ok(())
    }

    fn align(&mut self, fixture: &Value, replayed: &Value) -> Result<(), String> {
        match (fixture, replayed) {
            (Value::String(fixture), Value::String(replayed)) => {
                let (a, b) = (tokens(fixture), tokens(replayed));
                if a.len() == b.len() {
                    for (a, b) in a.into_iter().zip(b) {
                        self.learn(&fixture[a], &replayed[b])?;
                    }
                }
                Ok(())
            }
            (Value::Array(fixture), Value::Array(replayed)) if fixture.len() == replayed.len() => {
                fixture
                    .iter()
                    .zip(replayed)
                    .try_for_each(|(a, b)| self.align(a, b))
            }
            (Value::Object(fixture), Value::Object(replayed)) => {
                let mut unmatched: BTreeMap<&String, &Value> = BTreeMap::new();
                let mut left = Vec::new();
                for (key, value) in fixture {
                    match replayed.get(&self.rewrite_text(key)) {
                        Some(other) => self.align(value, other)?,
                        None => left.push((key, value)),
                    }
                }
                for (key, value) in replayed {
                    if !fixture.keys().any(|k| self.rewrite_text(k) == *key) {
                        unmatched.insert(key, value);
                    }
                }
                // Keys that are themselves new IDs pair up in order.
                if left.len() == unmatched.len() {
                    for ((key, value), (other_key, other)) in left.into_iter().zip(unmatched) {
                        self.align(
                            &Value::String(key.clone()),
                            &Value::String(other_key.clone()),
                        )?;
                        self.align(value, other)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn rewrite(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.rewrite_text(text)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.rewrite(item)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (self.rewrite_text(key), self.rewrite(value)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    pub(crate) fn rewrite_text(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for range in tokens(text) {
            out.push_str(&text[at..range.start]);
            let token = &text[range.clone()];
            out.push_str(self.0.get(token).map_or(token, String::as_str));
            at = range.end;
        }
        out.push_str(&text[at..]);
        out
    }
}

/// Byte ranges of the identity tokens in `text`: record IDs such as
/// `obs_<uuid>` and digests such as `sha256:<64 hex>`.
pub(crate) fn tokens(text: &str) -> Vec<Range<usize>> {
    const DIGEST: &[u8] = b"sha256:";
    let bytes = text.as_bytes();
    let hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !word(bytes[i - 1]);
        if boundary && bytes[i..].starts_with(DIGEST) {
            let end = i + DIGEST.len() + 64;
            if end <= bytes.len()
                && bytes[i + DIGEST.len()..end].iter().all(hex)
                && bytes.get(end).is_none_or(|b| !word(*b))
            {
                found.push(i..end);
                i = end;
                continue;
            }
        }
        if boundary && bytes[i].is_ascii_lowercase() {
            let prefix = bytes[i..]
                .iter()
                .take_while(|b| b.is_ascii_lowercase())
                .count();
            let end = i + prefix + 1 + 36;
            if bytes.get(i + prefix) == Some(&b'_')
                && end <= bytes.len()
                && bytes.get(end).is_none_or(|b| !word(*b))
                && text.get(i..end).is_some_and(|id| id.parse::<Id>().is_ok())
            {
                found.push(i..end);
                i = end;
                continue;
            }
            i += prefix;
            continue;
        }
        i += 1;
    }
    found
}
