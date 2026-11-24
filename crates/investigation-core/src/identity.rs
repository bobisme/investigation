//! Typed UUIDv7 identifiers. IDs identify records; they do not establish causality
//! or trusted time. Output always uses the full canonical form.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};
use uuid::{Uuid, Variant};

impl schemars::JsonSchema for Id {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Id".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"string", "pattern":"^(case|q|src|art|obs|hyp|asm|opt|crit|cmp|assess|concl|dec|rev|obj|ann|retro|inq|run|tx|handle|checkpoint|import|ws)_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"})
    }
}

/// Prefixes not explicitly illustrated in the plan are conventional and provisional.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Kind {
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
    Transaction,
    Handle,
    Checkpoint,
    Import,
    Workspace,
}
impl Kind {
    pub const ALL: &'static [Self] = &[
        Self::Case,
        Self::Question,
        Self::Source,
        Self::Artifact,
        Self::Observation,
        Self::Hypothesis,
        Self::Assumption,
        Self::Option,
        Self::Criterion,
        Self::Comparison,
        Self::Assessment,
        Self::Conclusion,
        Self::Decision,
        Self::Review,
        Self::Objection,
        Self::Annotation,
        Self::Retrospective,
        Self::Inquiry,
        Self::Run,
        Self::Transaction,
        Self::Handle,
        Self::Checkpoint,
        Self::Import,
        Self::Workspace,
    ];
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Case => "case",
            Self::Question => "q",
            Self::Source => "src",
            Self::Artifact => "art",
            Self::Observation => "obs",
            Self::Hypothesis => "hyp",
            Self::Assumption => "asm",
            Self::Option => "opt",
            Self::Criterion => "crit",
            Self::Comparison => "cmp",
            Self::Assessment => "assess",
            Self::Conclusion => "concl",
            Self::Decision => "dec",
            Self::Review => "rev",
            Self::Objection => "obj",
            Self::Annotation => "ann",
            Self::Retrospective => "retro",
            Self::Inquiry => "inq",
            Self::Run => "run",
            Self::Transaction => "tx",
            Self::Handle => "handle",
            Self::Checkpoint => "checkpoint",
            Self::Import => "import",
            Self::Workspace => "ws",
        }
    }
}
impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.prefix())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Id {
    kind: Kind,
    uuid: Uuid,
}
/// By kind, then by UUID bytes: the order a derive gives, with the UUID
/// compared as one big-endian number instead of byte by byte. Every map keyed
/// by ID compares IDs on each lookup.
impl Ord for Id {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.kind
            .cmp(&other.kind)
            .then_with(|| self.uuid.as_u128().cmp(&other.uuid.as_u128()))
    }
}
impl PartialOrd for Id {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdError {
    InvalidFormat,
    UnknownPrefix(String),
    InvalidUuid,
    WrongKind { expected: Kind, actual: Kind },
}
impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for IdError {}
impl Id {
    /// Every ID of `kind`, and no other, lies in this range: IDs order by
    /// kind first. Its bounds are ordering sentinels, not valid IDs.
    pub(crate) const fn kind_range(kind: Kind) -> std::ops::RangeInclusive<Self> {
        Self {
            kind,
            uuid: Uuid::from_u128(0),
        }..=Self {
            kind,
            uuid: Uuid::from_u128(u128::MAX),
        }
    }
    pub fn new(kind: Kind, uuid: Uuid) -> Result<Self, IdError> {
        if uuid.get_version_num() != 7 || uuid.get_variant() != Variant::RFC4122 {
            return Err(IdError::InvalidUuid);
        }
        Ok(Self { kind, uuid })
    }
    pub const fn kind(self) -> Kind {
        self.kind
    }
    pub const fn uuid(self) -> Uuid {
        self.uuid
    }
    pub fn expect_kind(self, expected: Kind) -> Result<Self, IdError> {
        if self.kind == expected {
            Ok(self)
        } else {
            Err(IdError::WrongKind {
                expected,
                actual: self.kind,
            })
        }
    }
}
impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", self.kind, self.uuid.hyphenated())
    }
}
impl FromStr for Id {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (prefix, value) = s.split_once('_').ok_or(IdError::InvalidFormat)?;
        let kind = Kind::ALL
            .iter()
            .copied()
            .find(|k| k.prefix() == prefix)
            .ok_or_else(|| IdError::UnknownPrefix(prefix.to_owned()))?;
        let uuid = Uuid::parse_str(value).map_err(|_| IdError::InvalidUuid)?;
        // Require one canonical spelling, not UUID parser aliases or uppercase.
        if value != uuid.hyphenated().to_string() {
            return Err(IdError::InvalidFormat);
        }
        Self::new(kind, uuid)
    }
}
impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}
impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Caller-owned source permits seeded deterministic generation without making
/// tests depend on wall time or global randomness. Implementations return v7 UUIDs.
pub trait IdSource {
    fn next_uuid(&mut self) -> Uuid;
    fn next_id(&mut self, kind: Kind) -> Result<Id, IdError> {
        Id::new(kind, self.next_uuid())
    }
}
/// System UUIDv7 generator; use an injected source for reproducible fixtures.
/// A [`crate::replay_seams::Replay`] on this thread supplies seeded IDs instead.
pub struct SystemIdSource;
impl IdSource for SystemIdSource {
    fn next_uuid(&mut self) -> Uuid {
        if let Some(uuid) = crate::replay_seams::next_uuid() {
            return uuid;
        }
        Uuid::now_v7()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    InvalidPrefix,
    NotFound,
    Ambiguous(Vec<Id>),
}
/// Resolve a typed, canonical textual prefix against known full IDs. Candidates
/// are sorted and deduplicated; callers must resolve before any mutation.
pub fn resolve_prefix(
    prefix: &str,
    candidates: impl IntoIterator<Item = Id>,
) -> Result<Id, ResolveError> {
    let (kind, fragment) = prefix.split_once('_').ok_or(ResolveError::InvalidPrefix)?;
    if !Kind::ALL.iter().any(|k| k.prefix() == kind)
        || fragment.is_empty()
        || !fragment
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
    {
        return Err(ResolveError::InvalidPrefix);
    }
    let mut matches: Vec<_> = candidates
        .into_iter()
        .filter(|id| id.to_string().starts_with(prefix))
        .collect();
    matches.sort();
    matches.dedup();
    match matches.len() {
        0 => Err(ResolveError::NotFound),
        1 => Ok(matches[0]),
        _ => Err(ResolveError::Ambiguous(matches)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_order_by_kind_then_uuid_bytes() {
        // UUIDs that differ in the first, a middle and the last byte, and at
        // the top bit of a byte, so a word-wise compare that broke byte order
        // would show.
        let uuids = [
            "01890f3e-7c00-7000-8000-000000000001",
            "01890f3e-7c00-7000-8000-000000000002",
            "01890f3e-7c00-7000-8000-000000000080",
            "01890f3e-7c00-7000-8000-000000000100",
            "01890f3e-7c00-7000-8000-800000000000",
            "01890f3e-7c00-7000-bfff-ffffffffffff",
            "01890f3f-0000-7000-8000-000000000000",
            "81890f3e-7c00-7000-8000-000000000000",
            "ff890f3e-7c00-7fff-8000-000000000000",
        ]
        .map(|text| Uuid::parse_str(text).unwrap());
        let ids: Vec<Id> = [Kind::Case, Kind::Option, Kind::Criterion, Kind::Workspace]
            .into_iter()
            .flat_map(|kind| uuids.iter().map(move |&uuid| Id::new(kind, uuid).unwrap()))
            .collect();
        for a in &ids {
            for b in &ids {
                let expected = (a.kind, a.uuid.as_bytes()).cmp(&(b.kind, b.uuid.as_bytes()));
                assert_eq!(a.cmp(b), expected, "{a} vs {b}");
                assert_eq!(a.partial_cmp(b), Some(expected));
                assert_eq!(a.cmp(b) == std::cmp::Ordering::Equal, a == b);
            }
        }
    }
    #[test]
    fn all_prefixes_round_trip_and_reject_wrong_kind() {
        let u = Uuid::parse_str("01890f3e-7c00-7000-8000-000000000001").unwrap();
        for &kind in Kind::ALL {
            let id = Id::new(kind, u).unwrap();
            assert_eq!(id.to_string().parse::<Id>().unwrap(), id);
            if kind != Kind::Case {
                assert!(id.expect_kind(Kind::Case).is_err());
            }
        }
        assert!(
            "case_01890f3e-7c00-7000-0000-000000000001"
                .parse::<Id>()
                .is_err()
        );
        assert!(
            "case_01890f3e-7c00-4000-8000-000000000001"
                .parse::<Id>()
                .is_err()
        );
    }
    #[test]
    fn resolution_is_unambiguous() {
        let a: Id = "case_01890f3e-7c00-7000-8000-000000000001".parse().unwrap();
        let b: Id = "case_01890f3e-7c00-7000-8000-000000000002".parse().unwrap();
        assert_eq!(
            resolve_prefix("case_0189", [b, a]),
            Err(ResolveError::Ambiguous(vec![a, b]))
        );
        assert_eq!(resolve_prefix(&a.to_string(), [a, b]), Ok(a));
    }
    #[test]
    fn injected_source_replays() {
        struct Seed(u128);
        impl IdSource for Seed {
            fn next_uuid(&mut self) -> Uuid {
                self.0 += 1;
                Uuid::from_u128(
                    (self.0 & 0xffff_ffff_ffff_0fff_3fff_ffff_ffff_ffff) | (7 << 76) | (2 << 62),
                )
            }
        }
        let mut a = Seed(42);
        let mut b = Seed(42);
        assert_eq!(a.next_id(Kind::Case), b.next_id(Kind::Case));
    }
    #[test]
    fn system_source_and_schema_use_v7() {
        assert_eq!(
            SystemIdSource.next_id(Kind::Case).unwrap().kind(),
            Kind::Case
        );
        let schema = serde_json::to_value(schemars::schema_for!(Id)).unwrap();
        assert!(
            schema["pattern"]
                .as_str()
                .unwrap()
                .contains("7[0-9a-f]{3}-[89ab]")
        );
    }
}
