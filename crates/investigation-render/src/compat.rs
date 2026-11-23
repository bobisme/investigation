//! The viewer compatibility profile: which release is pinned, which optional
//! features the renderer may use, and what a vault actually uses. Pure.
//!
//! The profile is recorded data (`obsidian/compatibility-profile.json`, with
//! the documentation revisions it was checked against). Nothing here contacts
//! the viewer or requires it. An absent, unparseable or out-of-range installed
//! release resolves to the baseline (Markdown notes and tables) without error.
//! [`detect`] reads vault bytes, so the check "every feature the renderer emits
//! is listed in the profile" runs offline against rendered vaults.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

const BUNDLED: &str = include_str!("../../../obsidian/compatibility-profile.json");

/// A dotted release number such as `1.15.1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Release(pub u32, pub u32, pub u32);

impl Release {
    /// Parses `1.15`, `1.15.1` or `v1.15.1`; anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches('v').split('.');
        let mut next = |required: bool| match parts.next() {
            Some(part) if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) => {
                part.parse().ok().map(Some)
            }
            None if !required => Some(Some(0)),
            _ => None,
        };
        let major = next(true)??;
        let minor = next(true)??;
        let patch = next(false)??;
        parts.next().is_none().then_some(Self(major, minor, patch))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PinnedRelease {
    pub version: String,
    pub released: String,
    /// `documentation` until an integration gate runs against the release.
    pub verification: String,
    pub integration_gate: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SupportedRange {
    pub min: String,
    pub max: String,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DocRevision {
    pub id: String,
    pub url: String,
    pub consulted: String,
    pub revision: String,
    pub observed: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Plain Markdown; needs no viewer capability.
    Baseline,
    /// Used only when the installed release is inside the supported range.
    Optional,
    /// Never emitted; listed so a future emitter must update the profile.
    Unsupported,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Feature {
    pub id: String,
    pub tier: Tier,
    pub emitted: bool,
    pub min_release: Option<String>,
    pub doc: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub schema: String,
    pub profile_revision: u32,
    pub pinned_release: PinnedRelease,
    pub supported_range: SupportedRange,
    pub documentation: Vec<DocRevision>,
    pub baseline: String,
    pub features: Vec<Feature>,
}

/// What the renderer may use for one installed release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// `None` when the release is absent, unparseable or outside the range.
    pub release: Option<String>,
    /// Optional features allowed; empty means the baseline.
    pub optional: BTreeSet<String>,
}

impl Profile {
    /// The profile built into this binary.
    pub fn bundled() -> Self {
        serde_json::from_str(BUNDLED).expect("bundled compatibility profile parses")
    }

    pub fn feature(&self, id: &str) -> Option<&Feature> {
        self.features.iter().find(|feature| feature.id == id)
    }

    fn range(&self) -> Option<(Release, Release)> {
        Some((
            Release::parse(&self.supported_range.min)?,
            Release::parse(&self.supported_range.max)?,
        ))
    }

    /// Chooses optional features for an installed release. Never fails.
    pub fn resolve(&self, installed: Option<&str>) -> Resolved {
        let baseline = Resolved {
            release: None,
            optional: BTreeSet::new(),
        };
        let Some(text) = installed else {
            return baseline;
        };
        let (Some(release), Some((min, max))) = (Release::parse(text), self.range()) else {
            return baseline;
        };
        if release < min || release > max {
            return baseline;
        }
        let optional = self
            .features
            .iter()
            .filter(|feature| feature.tier == Tier::Optional)
            .filter(|feature| {
                feature
                    .min_release
                    .as_deref()
                    .and_then(Release::parse)
                    .is_some_and(|needed| release >= needed)
            })
            .map(|feature| feature.id.clone())
            .collect();
        Resolved {
            release: Some(text.trim().to_owned()),
            optional,
        }
    }

    /// Features in `detected` the profile does not list as emitted-capable:
    /// unknown ids and `unsupported` ones.
    pub fn unlisted(&self, detected: &BTreeSet<String>) -> Vec<String> {
        detected
            .iter()
            .filter(|id| {
                self.feature(id)
                    .is_none_or(|feature| feature.tier == Tier::Unsupported)
            })
            .cloned()
            .collect()
    }

    /// Detected optional features the resolved release does not allow.
    pub fn gated(&self, detected: &BTreeSet<String>, resolved: &Resolved) -> Vec<String> {
        detected
            .iter()
            .filter(|id| {
                self.feature(id).is_some_and(|feature| {
                    feature.tier == Tier::Optional && !resolved.optional.contains(&feature.id)
                })
            })
            .cloned()
            .collect()
    }
}

/// The viewer features that the files of a vault use, from bytes alone.
pub fn detect<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (path, bytes) in files {
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        if path.ends_with(".md") {
            detect_markdown(text, &mut found);
        } else if path.ends_with(".base") {
            detect_base(text, &mut found);
        } else if path.ends_with(".canvas") {
            detect_canvas(text, &mut found);
        }
    }
    found
}

fn detect_markdown(note: &str, found: &mut BTreeSet<String>) {
    let mut body = note;
    if let Some(rest) = note.strip_prefix("---\n") {
        found.insert("md.frontmatter_properties".into());
        let (front, after) = rest.split_once("\n---\n").unwrap_or((rest, ""));
        for line in front.lines() {
            match line.split(':').next().unwrap_or("") {
                "aliases" => found.insert("md.aliases".into()),
                "tags" => found.insert("md.tags".into()),
                _ => false,
            };
        }
        body = after;
    }
    let mut in_fence = false;
    let mut previous_row = false;
    for line in body.lines() {
        if line.starts_with("```") || line.starts_with("~~~") {
            if !in_fence {
                found.insert("md.fenced_code".into());
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if line.contains("![[") {
            found.insert("md.embeds".into());
        }
        if line.contains("[[") {
            found.insert("md.wikilinks".into());
        }
        if line.starts_with("> [!") {
            found.insert("md.callouts".into());
        }
        if let Some((_, id)) = line.trim_end().rsplit_once(" ^")
            && !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            found.insert("md.block_anchors".into());
        }
        let delimiter = line.starts_with('|')
            && line.contains("---")
            && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '));
        if delimiter && previous_row {
            found.insert("md.tables".into());
        }
        previous_row = line.starts_with('|');
    }
}

fn detect_base(text: &str, found: &mut BTreeSet<String>) {
    found.insert("base.file".into());
    let mut views = false;
    for line in text.lines() {
        let key = line.trim_start_matches([' ', '-']);
        let top = !line.starts_with(' ');
        if key.starts_with("filters:") {
            found.insert("base.filters".into());
        }
        if key.starts_with("displayName:") {
            found.insert("base.property_display_name".into());
        }
        if top && key.starts_with("formulas:") {
            found.insert("base.formulas".into());
        }
        if top && key.starts_with("summaries:") {
            found.insert("base.summaries".into());
        }
        if top && key.starts_with("views:") {
            views = true;
        }
        if views {
            if key.starts_with("order:") {
                found.insert("base.view_order".into());
            }
            if key.starts_with("groupBy:") {
                found.insert("base.group_by".into());
            }
            if key.starts_with("sort:") || key.starts_with("sortBy:") {
                found.insert("base.row_sort_key".into());
            }
            if let Some(kind) = key.strip_prefix("type:") {
                found.insert(if kind.trim() == "table" {
                    "base.view_table".into()
                } else {
                    "base.view_other".into()
                });
            }
        }
    }
}

fn detect_canvas(text: &str, found: &mut BTreeSet<String>) {
    found.insert("canvas.file".into());
    let Ok(Value::Object(canvas)) = serde_json::from_str::<Value>(text) else {
        return;
    };
    for node in canvas
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        found.insert(match node.get("type").and_then(Value::as_str) {
            Some("file") => "canvas.file_nodes".into(),
            _ => "canvas.other_nodes".into(),
        });
    }
    for edge in canvas
        .get("edges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if edge.get("label").is_some() {
            found.insert("canvas.labeled_edges".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn the_profile_pins_an_exact_release_and_records_its_documentation() {
        let profile = Profile::bundled();
        let pinned = Release::parse(&profile.pinned_release.version).expect("exact release");
        assert!(pinned.2 > 0 || profile.pinned_release.version.matches('.').count() == 2);
        assert_eq!(profile.pinned_release.version, profile.supported_range.max);
        assert!(
            ["documentation", "integration"]
                .contains(&profile.pinned_release.verification.as_str())
        );
        for id in ["bases-syntax", "cli", "changelog"] {
            let doc = profile
                .documentation
                .iter()
                .find(|doc| doc.id == id)
                .unwrap();
            assert!(doc.url.starts_with("https://obsidian.md/"), "{id}");
            assert!(
                !doc.revision.is_empty() && doc.consulted.len() == 10,
                "{id}"
            );
        }
        // Every feature that cites a document cites one the profile records.
        for feature in &profile.features {
            if let Some(doc) = &feature.doc {
                assert!(
                    profile.documentation.iter().any(|d| &d.id == doc),
                    "{}",
                    feature.id
                );
            }
            if feature.tier == Tier::Optional {
                assert!(
                    feature
                        .min_release
                        .as_deref()
                        .and_then(Release::parse)
                        .is_some()
                );
            }
            if feature.tier == Tier::Unsupported {
                assert!(!feature.emitted, "{}", feature.id);
            }
        }
        let ids: BTreeSet<_> = profile.features.iter().map(|f| &f.id).collect();
        assert_eq!(ids.len(), profile.features.len(), "feature ids are unique");
    }

    #[test]
    fn an_absent_unknown_or_out_of_range_release_gets_the_baseline() {
        let profile = Profile::bundled();
        for installed in [
            None,
            Some(""),
            Some("not-a-version"),
            Some("1.x"),
            Some("1.12.6"),
            Some("0.9.0"),
            Some("1.15.2"),
            Some("2.0.0"),
        ] {
            let resolved = profile.resolve(installed);
            assert_eq!(resolved.release, None, "{installed:?}");
            assert!(resolved.optional.is_empty(), "{installed:?}");
        }
    }

    #[test]
    fn a_release_in_range_allows_only_optional_features_documented_for_it() {
        let profile = Profile::bundled();
        for installed in ["1.12.7", "v1.14.5", "1.15.1"] {
            let resolved = profile.resolve(Some(installed));
            assert!(resolved.release.is_some(), "{installed}");
            assert!(resolved.optional.contains("base.filters"));
            assert!(resolved.optional.contains("canvas.file_nodes"));
            // A release never unlocks what the documentation does not describe.
            assert!(!resolved.optional.contains("base.row_sort_key"));
            assert!(!resolved.optional.contains("base.view_other"));
            assert!(profile.gated(&set(&["base.filters"]), &resolved).is_empty());
        }
        let baseline = profile.resolve(None);
        assert_eq!(
            profile.gated(&set(&["base.filters", "md.tables"]), &baseline),
            ["base.filters"]
        );
    }

    #[test]
    fn detection_reads_markdown_base_and_canvas_bytes() {
        let note = "---\ninv_kind: \"case\"\naliases:\n  - \"x\"\n---\n# T\n\nSee [[a]].\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nline ^b-1\n\n```inv-excerpt\n[[not a link]]\n```\n";
        let base = "filters:\n  and:\n    - 'inv_kind == \"x\"'\nproperties:\n  inv_status:\n    displayName: \"S\"\nviews:\n  - type: table\n    name: V\n    sort:\n      - property: x\n    order:\n      - file.name\n";
        let canvas = r#"{"nodes":[{"type":"file","id":"a","file":"a.md"},{"type":"text","id":"b"}],"edges":[{"id":"e","label":"supports"}]}"#;
        let found = detect([
            ("a.md", note.as_bytes()),
            ("v.base", base.as_bytes()),
            ("m.canvas", canvas.as_bytes()),
        ]);
        for id in [
            "md.frontmatter_properties",
            "md.aliases",
            "md.wikilinks",
            "md.tables",
            "md.block_anchors",
            "md.fenced_code",
            "base.file",
            "base.filters",
            "base.property_display_name",
            "base.view_table",
            "base.view_order",
            "base.row_sort_key",
            "canvas.file",
            "canvas.file_nodes",
            "canvas.other_nodes",
            "canvas.labeled_edges",
        ] {
            assert!(found.contains(id), "{id} in {found:?}");
        }
        assert!(!found.contains("md.embeds") && !found.contains("md.tags"));
        let unlisted = Profile::bundled().unlisted(&found);
        assert_eq!(unlisted, ["base.row_sort_key", "canvas.other_nodes"]);
    }
}
