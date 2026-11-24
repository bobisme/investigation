//! Export identifiers: canonical, or remapped to opaque values for one
//! export (W11.13). Pure.
//!
//! Identifiers are metadata that travels with every export. A UUIDv7 record
//! ID embeds its creation time to the millisecond, and a case journal
//! sequence or head hash tells how many events the case holds, withheld ones
//! included. So the audience sees them only when it may:
//!
//! | Audience | Default | `--remap-ids` |
//! |---|---|---|
//! | `public` | remapped, always | remapped |
//! | `internal`, `restricted` | canonical | remapped |
//!
//! A **canonical** export names the case snapshot (case ID, sequence, head
//! hash) and shows record IDs as recorded; its manifest says that they embed
//! their creation time.
//!
//! A **remapped** export is rewritten after its kind has planned it, so no
//! kind needs code of its own for it:
//!
//! - Every UUIDv7 value in every file name and file body, in any letter
//!   case, prefixed (`obs_…`) or bare, record IDs and any quoted from
//!   evidence alike, becomes an opaque UUIDv7-shaped value: a keyed digest of
//!   the original whose time field is random and carries no creation time.
//!   The same original always gives the same value within the export, so
//!   links, file names, frontmatter and manifest references still agree.
//! - The manifest names the **bundle**, not the journal: the case ID is the
//!   remapped one, the sequence is 0 and the head hash is a bundle digest.
//!   The pinned head hash and the planner's generation ID, wherever a kind
//!   printed them, become the bundle values too.
//! - The key is a digest of the kind, the audience and the pinned case head,
//!   which the audience never sees, so the mapping cannot be recomputed or
//!   inverted from the export, and two kinds or audiences of one snapshot do
//!   not share IDs. The same snapshot, kind and audience always give the
//!   same values, so the export stays deterministic.
//! - The canonical snapshot and every pair of the mapping go only to the
//!   publisher ([`Identifiers`], the command's receipt), never into the
//!   export.
//! - The rewritten plan is checked before it is accepted: no head hash of the
//!   case's verified chain (pinned or today's) and no canonical ID in its
//!   32-digit form may remain. A kind that prints a journal sequence must
//!   take it from [`super::ExportInput::snapshot`], which is 0 when
//!   remapped; numbers cannot be checked generically.
//! - Revision numbers stay as recorded (bn-18la): a manifest `RevisionRef`
//!   pairs the remapped ID with the record's own revision, public exports
//!   included. Every kind's files cite records as "revision N" too, where
//!   the cited and the current revision tell a reader that a citation went
//!   stale, so a number dropped from the manifest alone would hide nothing
//!   and leave the manifest disagreeing with the files it lists; and v1
//!   requires `revision` ≥ 1, so no value could mean "not disclosed" as
//!   `sequence` 0 does. A revision number counts revisions of the shown
//!   record itself, never of another record: adding records that bear on
//!   nothing shown moves no shown number (the count-inference test covers
//!   remapped exports).
//! - Times the workspace stamped on records follow [`super::RecordTimes`]:
//!   the day only when remapped, none for public.
//! - A remapped export is a review product: it cannot restore canonical
//!   state, so a resumable kind refuses it.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use investigation_core::{command::RevisionRef, identity::Id, wire::Digest};
use investigation_store::journal::hash_bytes;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ExportError, KindSpec, ManifestSnapshot};
use crate::{Audience, CaseSnapshot, PlanBuilder, RenderPlan};

/// Version of the remapping: key derivation, value shape and the channels it
/// rewrites. Part of the key, so a change gives new values.
pub const REMAP_VERSION: &str = "inv.export-remap.v1";

/// Length of a hyphenated UUID.
const UUID_LEN: usize = 36;

/// Whether an export shows canonical identifiers or opaque ones.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierMode {
    /// Record IDs as recorded; they embed their creation time. The snapshot
    /// is the case journal's.
    Canonical,
    /// Opaque IDs assigned for this export only, with no creation time; the
    /// snapshot names the bundle, and the publisher keeps the mapping.
    Remapped,
}

impl IdentifierMode {
    /// The mode for `audience`: public is always remapped, and any audience
    /// is when `remap_ids` asks for it.
    pub const fn for_audience(audience: Audience, remap_ids: bool) -> Self {
        if remap_ids || matches!(audience, Audience::Public) {
            Self::Remapped
        } else {
            Self::Canonical
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::Remapped => "remapped",
        }
    }
}

/// What a kind may show of the export's identity: the snapshot the manifest
/// names and whether IDs are remapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportIdentity {
    pub mode: IdentifierMode,
    /// The case snapshot when canonical; the bundle when remapped.
    pub snapshot: ManifestSnapshot,
}

/// The publisher's side of an export's identifiers: the canonical snapshot
/// and, when remapped, every ID pair the export uses. Never written into the
/// export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifiers {
    mode: IdentifierMode,
    canonical: ManifestSnapshot,
    exported: ManifestSnapshot,
    /// Canonical value to exported value, as written: a full ID when it had
    /// a known prefix, the bare UUID otherwise.
    pairs: BTreeMap<String, String>,
    reverse: BTreeMap<String, String>,
    /// Exported UUID to canonical UUID, for bare values.
    uuids: BTreeMap<String, String>,
}

impl Identifiers {
    /// Canonical identifiers naming `snapshot`, for a plan that is never
    /// remapped (a workspace archive).
    pub(super) fn canonical_only(snapshot: ManifestSnapshot) -> Self {
        Self {
            mode: IdentifierMode::Canonical,
            canonical: snapshot.clone(),
            exported: snapshot,
            pairs: BTreeMap::new(),
            reverse: BTreeMap::new(),
            uuids: BTreeMap::new(),
        }
    }

    pub const fn mode(&self) -> IdentifierMode {
        self.mode
    }

    /// The case snapshot the export was rendered from.
    pub const fn canonical_snapshot(&self) -> &ManifestSnapshot {
        &self.canonical
    }

    /// The snapshot the export's manifest names.
    pub const fn exported_snapshot(&self) -> &ManifestSnapshot {
        &self.exported
    }

    /// Every pair, canonical to exported, in canonical order; empty when
    /// canonical.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs
            .iter()
            .map(|(canonical, exported)| (canonical.as_str(), exported.as_str()))
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// The value the export shows for a canonical ID or bare UUID; the same
    /// value when canonical.
    pub fn exported<'a>(&'a self, canonical: &'a str) -> Option<&'a str> {
        match self.mode {
            IdentifierMode::Canonical => Some(canonical),
            IdentifierMode::Remapped => {
                self.pairs.get(canonical).map(String::as_str).or_else(|| {
                    self.uuids
                        .iter()
                        .find(|(_, uuid)| *uuid == canonical)
                        .map(|(exported, _)| exported.as_str())
                })
            }
        }
    }

    /// The canonical ID or bare UUID behind a value the export shows.
    pub fn canonical<'a>(&'a self, exported: &'a str) -> Option<&'a str> {
        match self.mode {
            IdentifierMode::Canonical => Some(exported),
            IdentifierMode::Remapped => self
                .reverse
                .get(exported)
                .or_else(|| self.uuids.get(exported))
                .map(String::as_str),
        }
    }

    /// `bytes` of the export with every exported UUID put back to its
    /// canonical value, and the bundle head hash to the case head. For the
    /// publisher only.
    pub fn restore(&self, bytes: &[u8]) -> Vec<u8> {
        if self.mode == IdentifierMode::Canonical {
            return bytes.to_vec();
        }
        let restored = rewrite_uuids(bytes, |uuid, _| {
            Ok(self
                .uuids
                .get(uuid)
                .map_or_else(|| uuid.to_owned(), Clone::clone))
        })
        .unwrap_or_else(|_: ExportError| bytes.to_vec());
        replace_all(
            &restored,
            hex(&self.exported.head_hash).as_bytes(),
            hex(&self.canonical.head_hash).as_bytes(),
        )
    }
}

/// Builds an export's identifiers while it is planned.
pub(super) struct Remapper {
    mode: IdentifierMode,
    /// Hex key; empty when canonical.
    key: String,
    canonical: ManifestSnapshot,
    exported: ManifestSnapshot,
    pairs: BTreeMap<String, String>,
    reverse: BTreeMap<String, String>,
    /// Opaque UUID to canonical UUID, to refuse a collision.
    uuids: BTreeMap<String, String>,
}

impl Remapper {
    pub(super) fn new(
        kind: &KindSpec,
        case: &CaseSnapshot,
        audience: Audience,
        mode: IdentifierMode,
    ) -> Result<Self, ExportError> {
        let canonical = ManifestSnapshot {
            case_id: case.case_id(),
            sequence: case.head().sequence,
            head_hash: case.head().head_hash.clone(),
        };
        let mut remapper = Self {
            mode,
            key: String::new(),
            exported: canonical.clone(),
            canonical,
            pairs: BTreeMap::new(),
            reverse: BTreeMap::new(),
            uuids: BTreeMap::new(),
        };
        if mode == IdentifierMode::Canonical {
            return Ok(remapper);
        }
        if kind.product.resumable() {
            return Err(ExportError::RemapResumable { kind: kind.name });
        }
        remapper.key = hex(&hash_bytes(
            format!(
                "{REMAP_VERSION}\nkind {}\naudience {}\ncase {}\nsequence {}\nhead {}\n",
                kind.name,
                audience.as_str(),
                remapper.canonical.case_id,
                remapper.canonical.sequence,
                remapper.canonical.head_hash,
            )
            .as_bytes(),
        ))
        .to_owned();
        remapper.exported = ManifestSnapshot {
            case_id: remapper.id(remapper.canonical.case_id)?,
            sequence: 0,
            head_hash: remapper.derived("bundle"),
        };
        Ok(remapper)
    }

    pub(super) fn identity(&self) -> ExportIdentity {
        ExportIdentity {
            mode: self.mode,
            snapshot: self.exported.clone(),
        }
    }

    pub(super) fn exported_snapshot(&self) -> &ManifestSnapshot {
        &self.exported
    }

    fn remapped(&self) -> bool {
        self.mode == IdentifierMode::Remapped
    }

    /// A digest of the key and `label`: the bundle head and the like.
    fn derived(&self, label: &str) -> Digest {
        hash_bytes(format!("{}\n{label}\n", self.key).as_bytes())
    }

    /// The opaque UUID for a canonical one (lowercase, hyphenated).
    fn uuid(&mut self, canonical: &str) -> Result<String, ExportError> {
        let digest = hash_bytes(format!("{}\nuuid {canonical}\n", self.key).as_bytes());
        let mut digits: Vec<u8> = hex(&digest).as_bytes()[..32].to_vec();
        digits[12] = b'7';
        digits[16] = b"89ab"[usize::from(hex_value(digits[16]) & 0x3)];
        let digits = String::from_utf8(digits).expect("hex digits");
        let opaque = format!(
            "{}-{}-{}-{}-{}",
            &digits[..8],
            &digits[8..12],
            &digits[12..16],
            &digits[16..20],
            &digits[20..]
        );
        match self.uuids.get(&opaque) {
            Some(known) if known != canonical => {
                return Err(ExportError::IdentifierRefused {
                    path: String::new(),
                    reason: "two identifiers would share one remapped value",
                });
            }
            Some(_) => {}
            None => {
                self.uuids.insert(opaque.clone(), canonical.to_owned());
            }
        }
        Ok(opaque)
    }

    fn pair(&mut self, canonical: String, exported: String) {
        self.reverse.insert(exported.clone(), canonical.clone());
        self.pairs.insert(canonical, exported);
    }

    /// The ID the export shows for `id`.
    pub(super) fn id(&mut self, id: Id) -> Result<Id, ExportError> {
        if !self.remapped() {
            return Ok(id);
        }
        let canonical = id.to_string();
        let (prefix, uuid) = canonical
            .split_once('_')
            .expect("an ID has a prefix and a UUID");
        let opaque = format!("{prefix}_{}", self.uuid(uuid)?);
        let exported: Id = opaque
            .parse()
            .expect("a remapped ID keeps its prefix and UUIDv7 shape");
        self.pair(canonical, opaque);
        Ok(exported)
    }

    /// The reference the manifest shows: the remapped ID at the record's own
    /// revision, which is kept (see the module documentation).
    pub(super) fn reference(&mut self, reference: RevisionRef) -> Result<RevisionRef, ExportError> {
        Ok(RevisionRef {
            id: self.id(reference.id)?,
            revision: reference.revision,
        })
    }

    /// Rewrite every file name and body of `files`, then check that nothing
    /// canonical remains. `chain` lists every head hash the case's verified
    /// journal has had, pinned and today; `ids` every record ID of the case.
    /// Neither may appear.
    /// Every file a kind copied verbatim ([`Verbatim`]) must still hash to
    /// its recorded digest afterwards, or the plan is refused: a remap never
    /// ships altered evidence.
    pub(super) fn plan(
        &mut self,
        files: RenderPlan,
        chain: &[&Digest],
        ids: impl Iterator<Item = Id>,
        verbatim: &Verbatim,
    ) -> Result<RenderPlan, ExportError> {
        if !self.remapped() {
            verbatim.check(files.entries().iter().map(|entry| {
                (
                    entry.path().as_str(),
                    entry.path().as_str(),
                    entry.content(),
                )
            }))?;
            return Ok(files);
        }
        let head = (
            hex(&self.canonical.head_hash).to_owned(),
            hex(&self.exported.head_hash).to_owned(),
        );
        let generation_id = files.generation().id().to_owned();
        let generation = (
            generation_id.trim_start_matches("gen_").to_owned(),
            hex(&self.derived(&format!("generation {generation_id}"))).to_owned(),
        );
        let mut rewritten = Vec::with_capacity(files.entries().len());
        for entry in files.entries() {
            let path = self.text(entry.path().as_str().as_bytes(), &head, &generation)?;
            let path = String::from_utf8(path).expect("a rewritten path stays UTF-8");
            let content = self.text(entry.content(), &head, &generation)?;
            let projects = entry.projects().map(|id| self.id(id)).transpose()?;
            rewritten.push((
                entry.phase(),
                crate::PlanPath::new(path)?,
                content,
                projects,
            ));
        }
        let mut canonical: BTreeSet<String> =
            ids.map(|id| id.uuid().simple().to_string()).collect();
        canonical.extend(
            self.pairs
                .keys()
                .filter_map(|value| uuid_part(value))
                .map(|uuid| uuid.replace('-', "")),
        );
        let mut runs = Runs::default();
        for (_, path, content, _) in &rewritten {
            runs.add(path.as_str(), path.as_str().as_bytes());
            runs.add(path.as_str(), content);
        }
        runs.check(chain, &canonical)?;
        verbatim.check(files.entries().iter().zip(&rewritten).map(
            |(entry, (_, path, content, _))| {
                (entry.path().as_str(), path.as_str(), content.as_slice())
            },
        ))?;
        let mut builder = PlanBuilder::new(files.generation().clone());
        for (phase, path, content, projects) in rewritten {
            builder.add(phase, path, content, projects)?;
        }
        Ok(builder.finish())
    }

    fn text(
        &mut self,
        bytes: &[u8],
        head: &(String, String),
        generation: &(String, String),
    ) -> Result<Vec<u8>, ExportError> {
        let mut found = Vec::new();
        let rewritten = rewrite_uuids(bytes, |uuid, prefix| {
            // A value the kind took from `ExportInput::snapshot` is already
            // remapped; rewriting is idempotent.
            if self.uuids.contains_key(uuid) {
                return Ok(uuid.to_owned());
            }
            let opaque = self.uuid(uuid)?;
            found.push((uuid.to_owned(), opaque.clone(), prefix.map(str::to_owned)));
            Ok(opaque)
        })?;
        for (uuid, opaque, prefix) in found {
            match prefix {
                Some(prefix) => self.pair(format!("{prefix}_{uuid}"), format!("{prefix}_{opaque}")),
                None => self.pair(uuid, opaque),
            }
        }
        let rewritten = replace_all(&rewritten, head.0.as_bytes(), head.1.as_bytes());
        Ok(replace_all(
            &rewritten,
            generation.0.as_bytes(),
            generation.1.as_bytes(),
        ))
    }

    pub(super) fn finish(self) -> Identifiers {
        Identifiers {
            mode: self.mode,
            canonical: self.canonical,
            exported: self.exported,
            pairs: self.pairs,
            reverse: self.reverse,
            uuids: self.uuids,
        }
    }
}

/// The files a kind copied byte for byte from retained evidence, by planned
/// path, with the digest they must keep (bn-397s). A kind reports each
/// through [`super::ExportInput::copied_verbatim`]; the remapper checks them
/// after rewriting.
#[derive(Debug, Default)]
pub struct Verbatim(RefCell<BTreeMap<String, Digest>>);

impl Verbatim {
    pub fn new() -> Self {
        Self::default()
    }

    pub(super) fn report(&self, path: &str, digest: Digest) {
        self.0.borrow_mut().insert(path.to_owned(), digest);
    }

    /// `entries` as (planned path, written path, written bytes): every
    /// reported path must be written and hash to its digest.
    fn check<'e>(
        &self,
        entries: impl Iterator<Item = (&'e str, &'e str, &'e [u8])>,
    ) -> Result<(), ExportError> {
        let expected = self.0.borrow();
        let mut seen = 0;
        for (planned, written, bytes) in entries {
            let Some(digest) = expected.get(planned) else {
                continue;
            };
            seen += 1;
            if &hash_bytes(bytes) != digest {
                return Err(ExportError::IdentifierRefused {
                    path: written.to_owned(),
                    reason: "bytes copied verbatim would no longer match their recorded digest",
                });
            }
        }
        if seen == expected.len() {
            Ok(())
        } else {
            Err(ExportError::IdentifierRefused {
                path: String::new(),
                reason: "a file reported as copied verbatim is not in the plan",
            })
        }
    }
}

/// Whether `bytes` may hold an identifier a remap would rewrite: a run of
/// 32 or more hex digits (a 32-digit ID, a digest, a head hash) or a
/// UUID-shaped value in any letter case. Bytes that hold none are left
/// unchanged by a remap.
pub(crate) fn may_hold_identifier(bytes: &[u8]) -> bool {
    let mut run = 0usize;
    for byte in bytes {
        run = if byte.is_ascii_hexdigit() { run + 1 } else { 0 };
        if run >= 32 {
            return true;
        }
    }
    bytes.windows(UUID_LEN).any(|window| {
        window.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
    })
}

/// The 64- and 32-digit hex runs of a plan, each with the first file that
/// holds it, so the plan is checked against the case once, not per file.
#[derive(Default)]
struct Runs {
    digests: BTreeMap<String, String>,
    simple: BTreeMap<String, String>,
}

impl Runs {
    fn add(&mut self, path: &str, bytes: &[u8]) {
        for (length, found) in [(64, &mut self.digests), (32, &mut self.simple)] {
            for run in hex_runs(bytes, length) {
                found.entry(run).or_insert_with(|| path.to_owned());
            }
        }
    }

    /// Refuse when a verified head hash of the case, or the 32-digit form
    /// of a canonical UUID, remains.
    fn check(&self, chain: &[&Digest], canonical: &BTreeSet<String>) -> Result<(), ExportError> {
        let refused = |path: &String, reason| {
            Err(ExportError::IdentifierRefused {
                path: path.clone(),
                reason,
            })
        };
        if !self.digests.is_empty()
            && let Some(path) = chain
                .iter()
                .find_map(|digest| self.digests.get(hex(digest)))
        {
            return refused(
                path,
                "a case journal head hash would remain in a remapped export",
            );
        }
        if let Some(path) = self
            .simple
            .iter()
            .find_map(|(run, path)| canonical.contains(run).then_some(path))
        {
            return refused(
                path,
                "a canonical identifier would remain in a remapped export",
            );
        }
        Ok(())
    }
}

/// The lowercase runs of exactly `length` hex digits, bounded by non-hex
/// letters or digits.
fn hex_runs(bytes: &[u8], length: usize) -> BTreeSet<String> {
    let mut runs = BTreeSet::new();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_hexdigit() {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
            end += 1;
        }
        let bounded = (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
            && (end == bytes.len() || !bytes[end].is_ascii_alphanumeric());
        if end - start == length && bounded {
            runs.insert(String::from_utf8_lossy(&bytes[start..end]).to_ascii_lowercase());
        }
        start = end;
    }
    runs
}

/// The hex digits of a digest, without `sha256:`.
fn hex(digest: &Digest) -> &str {
    digest.as_str().trim_start_matches("sha256:")
}

fn hex_value(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        b'A'..=b'F' => digit - b'A' + 10,
        _ => 0,
    }
}

/// The UUID of a full ID or a bare UUID.
fn uuid_part(value: &str) -> Option<&str> {
    let uuid = value.rsplit_once('_').map_or(value, |(_, uuid)| uuid);
    (uuid.len() == UUID_LEN).then_some(uuid)
}

/// Whether `bytes` is a hyphenated UUIDv7 in either letter case.
fn is_uuid7(bytes: &[u8]) -> bool {
    bytes.len() == UUID_LEN
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'7',
            19 => matches!(byte.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_hexdigit(),
        })
}

/// The ID prefix directly before `at` (`obs` in `obs_<uuid>`), when it is a
/// known one.
fn prefix_before(bytes: &[u8], at: usize) -> Option<&str> {
    let underscore = at.checked_sub(1).filter(|index| bytes[*index] == b'_')?;
    let mut start = underscore;
    while start > 0 && bytes[start - 1].is_ascii_lowercase() {
        start -= 1;
    }
    if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
        return None;
    }
    let prefix = std::str::from_utf8(&bytes[start..underscore]).ok()?;
    investigation_core::identity::Kind::ALL
        .iter()
        .any(|kind| kind.prefix() == prefix)
        .then_some(prefix)
}

/// `bytes` with every bounded hyphenated UUIDv7 replaced by `map` of its
/// lowercase form and its ID prefix, if any.
fn rewrite_uuids(
    bytes: &[u8],
    mut map: impl FnMut(&str, Option<&str>) -> Result<String, ExportError>,
) -> Result<Vec<u8>, ExportError> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let end = index + UUID_LEN;
        if end <= bytes.len()
            && is_uuid7(&bytes[index..end])
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
            && (end == bytes.len() || !bytes[end].is_ascii_alphanumeric())
        {
            let uuid = String::from_utf8_lossy(&bytes[index..end]).to_ascii_lowercase();
            let replacement = map(&uuid, prefix_before(bytes, index))?;
            out.extend_from_slice(replacement.as_bytes());
            index = end;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Ok(out)
}

/// `bytes` with every occurrence of `from`, in either letter case, replaced
/// by `to`.
fn replace_all(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() || bytes.len() < from.len() {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes.len() - index >= from.len()
            && bytes[index..index + from.len()].eq_ignore_ascii_case(from)
        {
            out.extend_from_slice(to);
            index += from.len();
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbatim_copy_must_be_planned_and_keep_its_digest() {
        let verbatim = Verbatim::new();
        verbatim.report("runs/0001/files/out.json", hash_bytes(b"exact\n"));
        let path = "runs/0001/files/out.json";
        assert!(
            verbatim
                .check([(path, path, &b"exact\n"[..])].into_iter())
                .is_ok()
        );
        // Rewritten bytes, or a copy that is not planned at all, refuse.
        assert!(matches!(
            verbatim.check([(path, path, &b"altered\n"[..])].into_iter()),
            Err(ExportError::IdentifierRefused { path: written, .. }) if written == path
        ));
        assert!(verbatim.check(std::iter::empty()).is_err());
        assert!(Verbatim::new().check(std::iter::empty()).is_ok());
    }

    #[test]
    fn bytes_that_hold_no_identifier_are_told_apart_from_those_that_may() {
        assert!(!may_hold_identifier(
            b"{\"metric\":\"latency.p99\",\"value\":18.7}"
        ));
        assert!(may_hold_identifier(
            b"x 019acd16-0003-7000-8000-000000000001 y"
        ));
        assert!(may_hold_identifier(b"0123456789ABCDEF0123456789abcdef"));
        assert!(!may_hold_identifier(b"0123456789abcdef0123456789abcde"));
    }

    const OBS: &str = "01a20751-8e56-7b21-9630-4b49275ec85d";
    const CASE: &str = "01a20751-9598-7d12-a156-045a6355965c";

    fn remapper(key: &str) -> Remapper {
        let canonical = ManifestSnapshot {
            case_id: format!("case_{CASE}").parse().unwrap(),
            sequence: 12,
            head_hash: hash_bytes(b"head"),
        };
        Remapper {
            mode: IdentifierMode::Remapped,
            key: key.to_owned(),
            exported: canonical.clone(),
            canonical,
            pairs: BTreeMap::new(),
            reverse: BTreeMap::new(),
            uuids: BTreeMap::new(),
        }
    }

    fn rewrite(remapper: &mut Remapper, text: &str) -> String {
        let none = (String::from("-"), String::from("-"));
        String::from_utf8(remapper.text(text.as_bytes(), &none, &none).unwrap()).unwrap()
    }

    #[test]
    fn every_form_of_one_uuid_gets_one_opaque_value_and_links_still_agree() {
        let mut remapper = remapper("k1");
        let note = format!(
            "[[Records/obs_{OBS}|title]] obs_{OBS} {OBS} OBS_{upper} `{upper}` see ref:{OBS}.",
            upper = OBS.to_ascii_uppercase()
        );
        let path = format!("Records/obs_{OBS}.md");
        let rewritten = rewrite(&mut remapper, &note);
        let rewritten_path = rewrite(&mut remapper, &path);
        assert!(!rewritten.to_ascii_lowercase().contains(OBS), "{rewritten}");
        let opaque = remapper.pairs[&format!("obs_{OBS}")].clone();
        let bare = uuid_part(&opaque).unwrap().to_owned();
        assert_eq!(remapper.pairs[OBS], bare);
        assert_eq!(rewritten_path, format!("Records/{opaque}.md"));
        assert!(rewritten.contains(&format!("[[Records/{opaque}|title]]")));
        assert_eq!(rewritten.matches(&bare).count(), 6, "{rewritten}");
        // The remapped value is a valid ID with no trace of the original time.
        let id: Id = opaque.parse().unwrap();
        assert_eq!(id.to_string(), opaque);
        assert!(!opaque.contains("01a20751"));
        // The private side puts it back.
        let identifiers = remapper.finish();
        assert_eq!(identifiers.canonical(&opaque), Some(&*format!("obs_{OBS}")));
        let restored = String::from_utf8(identifiers.restore(rewritten.as_bytes())).unwrap();
        assert!(restored.contains(&format!("[[Records/obs_{OBS}|title]]")));
        assert!(!restored.contains(&bare));
    }

    #[test]
    fn values_depend_on_the_key_and_nothing_else() {
        let text = format!("case_{CASE} obs_{OBS}");
        let first = rewrite(&mut remapper("k1"), &text);
        assert_eq!(first, rewrite(&mut remapper("k1"), &text));
        assert_ne!(first, rewrite(&mut remapper("k2"), &text));
    }

    #[test]
    fn a_value_already_remapped_is_left_as_it_is() {
        let mut remapper = remapper("k1");
        let case: Id = format!("case_{CASE}").parse().unwrap();
        let shown = remapper.id(case).unwrap().to_string();
        let text = format!("{shown} and case_{CASE}");
        assert_eq!(
            rewrite(&mut remapper, &text),
            format!("{shown} and {shown}")
        );
    }

    #[test]
    fn only_bounded_uuidv7_values_are_rewritten() {
        let mut remapper = remapper("k1");
        let v4 = "1b4e28ba-2fa1-41d2-883f-0016d3cca427";
        let embedded = format!("x{OBS}");
        let longer = format!("{OBS}0");
        for text in [v4, embedded.as_str(), longer.as_str(), "not an id"] {
            assert_eq!(rewrite(&mut remapper, text), text);
        }
        assert!(remapper.pairs.is_empty());
    }

    #[test]
    fn a_head_hash_or_a_32_digit_canonical_id_left_behind_is_refused() {
        let canonical = BTreeSet::from([OBS.replace('-', "")]);
        let head = hash_bytes(b"older head");
        let check = |text: &str, chain: &[&Digest]| {
            let mut runs = Runs::default();
            runs.add("a.md", text.as_bytes());
            runs.check(chain, &canonical)
        };
        let error = check(&format!("reviewed at {head}"), &[&head]).unwrap_err();
        assert!(matches!(error, ExportError::IdentifierRefused { ref path, .. } if path == "a.md"));
        assert!(check(&format!("id {}", OBS.replace('-', "")), &[]).is_err());
        assert!(check("nothing here", &[&head]).is_ok());
    }
}
