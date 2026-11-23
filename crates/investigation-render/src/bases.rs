//! Native Base files: the dynamic equivalent of the static table views
//! (plan §12.5). Pure.
//!
//! A Base is a YAML file a viewer reads to show a sortable, filterable table
//! of notes by their flat `inv_` properties. One Base is planned beside each
//! static table note under `Cases/<case-id>/Views/<name>.base`, except the
//! comparison matrix, which stays a static table in v1.
//!
//! - Optional. Nothing here is needed to read the vault: the static tables
//!   hold the same rows. A Base is planned only when the view policy allows
//!   every feature in [`FEATURES`] ([`crate::ViewPolicy::allows_all`]), which
//!   the working vault's policy does only when the stated viewer release
//!   resolves to them in the compatibility profile ([`allowed`]). An absent
//!   or unknown release gets no Base files at all.
//! - Exact case. Every Base filters `inv_case_id == "<case-id>"` and the
//!   record kinds of its view, never a folder, so a note of another case
//!   never appears in it.
//! - Same rows. Each filter selects exactly the notes whose records are the
//!   rows of the matching static table, from properties every such note
//!   carries (placeholders of withheld records included): `inv_kind`,
//!   `inv_value_kind` on observations and `inv_open_gaps`.
//! - Documented syntax only: `filters` with `and`/`or`, `properties` with
//!   `displayName`, and one `table` view with `order`. No row sort key, no
//!   formula, summary or other view type ([`crate::compat`] lists them).
//! - Stable bytes. The content depends only on the case ID and the view, so
//!   identical renders, and renders at later snapshots of the same case,
//!   give identical Base bytes. Strings use the tested
//!   [`crate::yaml::quoted`] escaper; keys are code constants.
//! - Tool-owned. A Base is in the ownership manifest like every generated
//!   file, so an edit to it is drift. Editing a cell in a Base changes the
//!   underlying note's properties, which is drift on that note.

use std::fmt::Write as _;

use investigation_core::identity::{Id, Kind};

use crate::{
    PlanPath, RenderError,
    compat::Resolved,
    layout::{self, kind_name},
    yaml::quoted,
};

/// The profile features every planned Base uses. All must be allowed for
/// the release before any Base is planned.
pub const FEATURES: &[&str] = &[
    "base.file",
    "base.filters",
    "base.property_display_name",
    "base.view_table",
    "base.view_order",
];

/// Whether a resolved viewer release allows every Base feature.
pub fn allowed(resolved: &Resolved) -> bool {
    FEATURES
        .iter()
        .all(|feature| resolved.optional.contains(*feature))
}

/// One filter statement, or a group of them.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Filter {
    /// `inv_kind == "<kind>"`.
    Kind(Kind),
    /// `<property> == "<word>"`.
    Is(&'static str, &'static str),
    /// `<property> != "<word>"`.
    IsNot(&'static str, &'static str),
    /// `<property> > 0`.
    Positive(&'static str),
    All(&'static [Filter]),
    Any(&'static [Filter]),
}

/// One shipped Base: the view slug it belongs to, the filter after the case
/// filter, and its columns as property and header label.
pub(crate) struct Spec {
    pub slug: &'static str,
    pub filter: Filter,
    pub columns: &'static [(&'static str, &'static str)],
}

const ID: (&str, &str) = ("file.name", "Record ID");
const TITLE: (&str, &str) = ("aliases", "Title");
const STATUS: (&str, &str) = ("inv_status", "Status");
const EFFECTIVE: (&str, &str) = ("inv_effective", "Effective state");
const OPEN_GAPS: (&str, &str) = ("inv_open_gaps", "Open gaps");
const KIND: (&str, &str) = ("inv_kind", "Kind");
const REVIEWED: (&str, &str) = ("inv_review_freshness", "Review freshness");
const REVISION: (&str, &str) = ("inv_revision", "Rev");

/// The shipped Bases in §12.5 order. The comparison matrix has none.
pub(crate) const SPECS: &[Spec] = &[
    Spec {
        slug: "questions",
        filter: Filter::Kind(Kind::Question),
        columns: &[ID, TITLE, STATUS, EFFECTIVE, OPEN_GAPS, REVISION],
    },
    Spec {
        slug: "hypotheses",
        filter: Filter::Kind(Kind::Hypothesis),
        columns: &[
            ID,
            TITLE,
            ("inv_status", "Standing"),
            EFFECTIVE,
            REVIEWED,
            OPEN_GAPS,
            REVISION,
        ],
    },
    Spec {
        slug: "options",
        filter: Filter::Kind(Kind::Option),
        columns: &[ID, TITLE, STATUS, EFFECTIVE, OPEN_GAPS, REVISION],
    },
    Spec {
        slug: "criteria",
        filter: Filter::Kind(Kind::Criterion),
        columns: &[ID, TITLE, STATUS, EFFECTIVE, OPEN_GAPS, REVISION],
    },
    Spec {
        slug: "sources",
        filter: Filter::Kind(Kind::Source),
        columns: &[
            ID,
            TITLE,
            STATUS,
            ("inv_source_access", "Access"),
            ("inv_assessed", "Assessed"),
            EFFECTIVE,
            REVISION,
        ],
    },
    Spec {
        slug: "evidence",
        filter: Filter::Any(&[
            Filter::Kind(Kind::Artifact),
            Filter::All(&[
                Filter::Kind(Kind::Observation),
                Filter::IsNot("inv_value_kind", MEASUREMENT),
            ]),
        ]),
        columns: &[
            ID,
            TITLE,
            KIND,
            ("inv_artifact_role", "Role"),
            ("inv_integrity", "Integrity"),
            ("inv_availability", "Availability"),
            ("inv_selector_validation", "Selector"),
            ("inv_evidentiary_use", "Evidentiary use"),
            REVISION,
        ],
    },
    Spec {
        slug: "measurements",
        filter: Filter::All(&[
            Filter::Kind(Kind::Observation),
            Filter::Is("inv_value_kind", MEASUREMENT),
        ]),
        columns: &[
            ID,
            TITLE,
            STATUS,
            ("inv_basis", "Basis"),
            ("inv_selector_validation", "Selector"),
            ("inv_evidentiary_use", "Evidentiary use"),
            EFFECTIVE,
            REVISION,
        ],
    },
    Spec {
        slug: "inquiries_and_runs",
        filter: Filter::Any(&[Filter::Kind(Kind::Inquiry), Filter::Kind(Kind::Run)]),
        columns: &[ID, TITLE, KIND, STATUS, EFFECTIVE, OPEN_GAPS, REVISION],
    },
    Spec {
        slug: "conclusions",
        filter: Filter::Kind(Kind::Conclusion),
        columns: &[
            ID,
            TITLE,
            EFFECTIVE,
            ("inv_disposition", "Disposition"),
            STATUS,
            REVIEWED,
            REVISION,
        ],
    },
    Spec {
        slug: "decisions",
        filter: Filter::Kind(Kind::Decision),
        columns: &[
            ID,
            TITLE,
            ("inv_disposition", "Effective decision"),
            STATUS,
            EFFECTIVE,
            REVIEWED,
            REVISION,
        ],
    },
    Spec {
        slug: "reviews",
        filter: Filter::Kind(Kind::Review),
        columns: &[
            ID,
            TITLE,
            ("inv_review_kind", "Review kind"),
            ("inv_verdict", "Verdict"),
            ("inv_freshness", "Freshness"),
            ("inv_basis_sequence", "Basis sequence"),
            STATUS,
            REVISION,
        ],
    },
    Spec {
        slug: "retrospectives",
        filter: Filter::Kind(Kind::Retrospective),
        columns: &[
            ID,
            TITLE,
            STATUS,
            ("inv_freshness", "Freshness"),
            ("inv_basis_sequence", "Basis sequence"),
            REVISION,
        ],
    },
    Spec {
        slug: "frontier",
        // View notes carry the case ID but no gap count.
        filter: Filter::All(&[
            Filter::IsNot("inv_kind", "view"),
            Filter::Positive("inv_open_gaps"),
        ]),
        columns: &[ID, TITLE, KIND, OPEN_GAPS, EFFECTIVE, STATUS],
    },
    Spec {
        slug: "timeline",
        filter: Filter::IsNot("inv_kind", "view"),
        columns: &[
            ID,
            TITLE,
            KIND,
            REVISION,
            ("inv_updated", "Recorded"),
            STATUS,
        ],
    },
];

/// The `inv_value_kind` word of a measured observation.
pub(crate) const MEASUREMENT: &str = "measurement";

/// The Base spec of a view, or `None` for a view without one.
pub(crate) fn spec(slug: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|spec| spec.slug == slug)
}

/// The planned Base of one view of one case: path and bytes.
pub(crate) fn plan(
    case_id: Id,
    name: &str,
    spec: &Spec,
) -> Result<(PlanPath, String), RenderError> {
    let path = layout::case_base(case_id, name)?;
    let table = layout::case_view(case_id, name)?;
    Ok((path, content(case_id, name, table.as_str(), spec)))
}

fn content(case_id: Id, name: &str, table: &str, spec: &Spec) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Tool-owned projection generated by Investigation: the {name} view of one case."
    );
    let _ = writeln!(out, "# The static table with the same rows: {table}");
    out.push_str(
        "# Editing this file, or a cell in its table (which changes a note's properties), \
         is drift.\n",
    );
    out.push_str("filters:\n  and:\n");
    item(
        &mut out,
        4,
        &format!("inv_case_id == \"{}\"", word(&case_id.to_string())),
    );
    filter(&mut out, 4, spec.filter);
    out.push_str("properties:\n");
    for (property, label) in spec.columns {
        let _ = writeln!(out, "  {}:", property_id(property));
        out.push_str("    displayName: ");
        quoted(&mut out, label);
        out.push('\n');
    }
    out.push_str("views:\n  - type: table\n    name: ");
    quoted(&mut out, name);
    out.push_str("\n    order:\n");
    for (property, _) in spec.columns {
        let _ = writeln!(out, "      - {}", property_id(property));
    }
    out
}

/// One filter as a list item at `indent` spaces.
fn filter(out: &mut String, indent: usize, filter: Filter) {
    let statement = match filter {
        Filter::Kind(kind) => format!("inv_kind == \"{}\"", kind_name(kind)),
        Filter::Is(property, value) => format!("{} == \"{}\"", property_id(property), word(value)),
        Filter::IsNot(property, value) => {
            format!("{} != \"{}\"", property_id(property), word(value))
        }
        Filter::Positive(property) => format!("{} > 0", property_id(property)),
        Filter::All(group) | Filter::Any(group) => {
            let key = if matches!(filter, Filter::All(_)) {
                "and"
            } else {
                "or"
            };
            let _ = writeln!(out, "{:indent$}- {key}:", "");
            for member in group {
                self::filter(out, indent + 4, *member);
            }
            return;
        }
    };
    item(out, indent, &statement);
}

fn item(out: &mut String, indent: usize, statement: &str) {
    let _ = write!(out, "{:indent$}- ", "");
    quoted(out, statement);
    out.push('\n');
}

/// A property ID: lowercase letters, digits, underscores and one optional
/// `file.` prefix. Columns are code constants; anything else is a bug.
fn property_id(property: &str) -> &str {
    let name = property.strip_prefix("file.").unwrap_or(property);
    assert!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
        "invalid Base property {property:?}"
    );
    property
}

/// A value inside a filter string: an ID or a fixed word, never authored
/// text, so it can never close its own quotes.
fn word(value: &str) -> &str {
    assert!(
        !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
        "invalid Base filter value {value:?}"
    );
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compat::{Profile, Tier, detect},
        records::views::VIEWS,
    };

    fn case() -> Id {
        "case_019acd16-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn parse(text: &str) -> yaml_serde::Mapping {
        yaml_serde::from_str(text).expect("a Base parses as YAML")
    }

    #[test]
    fn every_view_but_the_matrix_has_one_base() {
        let with: Vec<&str> = VIEWS
            .iter()
            .filter(|(_, slug)| spec(slug).is_some())
            .map(|(_, slug)| *slug)
            .collect();
        assert_eq!(with.len(), 14);
        assert_eq!(SPECS.len(), 14);
        assert!(spec("comparison_matrix").is_none());
        for spec in SPECS {
            assert!(VIEWS.iter().any(|(_, slug)| *slug == spec.slug));
            assert_eq!(spec.columns[0], ID, "{}", spec.slug);
        }
    }

    #[test]
    fn a_base_filters_by_the_exact_case_and_kind_in_documented_syntax() {
        let (path, text) = plan(case(), "Conclusions", spec("conclusions").unwrap()).unwrap();
        assert_eq!(
            path.as_str(),
            "Cases/case_019acd16-0000-7000-8000-000000000001/Views/Conclusions.base"
        );
        let map = parse(&text);
        let keys: Vec<&str> = map.keys().map(|key| key.as_str().unwrap()).collect();
        assert_eq!(keys, ["filters", "properties", "views"]);
        let and = map["filters"]["and"].as_sequence().unwrap();
        assert_eq!(
            and[0],
            "inv_case_id == \"case_019acd16-0000-7000-8000-000000000001\""
        );
        assert_eq!(and[1], "inv_kind == \"conclusion\"");
        assert_eq!(
            map["properties"]["inv_effective"]["displayName"],
            "Effective state"
        );
        let views = map["views"].as_sequence().unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0]["type"], "table");
        assert_eq!(views[0]["name"], "Conclusions");
        let order: Vec<&str> = views[0]["order"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert_eq!(order[..3], ["file.name", "aliases", "inv_effective"]);
        // Every column has a label; nothing else is configured.
        let properties = map["properties"].as_mapping().unwrap();
        assert_eq!(properties.len(), order.len());

        // Groups nest as and/or objects.
        let (_, evidence) = plan(case(), "Evidence", spec("evidence").unwrap()).unwrap();
        let map = parse(&evidence);
        let any = &map["filters"]["and"][1]["or"];
        assert_eq!(any[0], "inv_kind == \"artifact\"");
        assert_eq!(any[1]["and"][0], "inv_kind == \"observation\"");
        assert_eq!(any[1]["and"][1], "inv_value_kind != \"measurement\"");
    }

    #[test]
    fn bases_use_only_features_the_profile_lists_as_emitted_optional() {
        let profile = Profile::bundled();
        let mut files = Vec::new();
        for (name, slug) in VIEWS {
            if let Some(spec) = spec(slug) {
                files.push(plan(case(), name, spec).unwrap());
            }
        }
        let found = detect(
            files
                .iter()
                .map(|(path, text)| (path.as_str(), text.as_bytes())),
        );
        let expected: std::collections::BTreeSet<String> =
            FEATURES.iter().map(|id| (*id).to_owned()).collect();
        assert_eq!(found, expected);
        for id in FEATURES {
            let feature = profile.feature(id).unwrap();
            assert_eq!(feature.tier, Tier::Optional, "{id}");
            assert!(feature.emitted, "{id}");
        }
        assert!(allowed(
            &profile.resolve(Some(&profile.pinned_release.version))
        ));
        assert!(allowed(
            &profile.resolve(Some(&profile.supported_range.min))
        ));
        for installed in [None, Some("garbage"), Some("0.9.0"), Some("99.0.0")] {
            assert!(!allowed(&profile.resolve(installed)), "{installed:?}");
        }
    }

    #[test]
    fn base_bytes_depend_only_on_the_case_and_the_view() {
        let spec = spec("timeline").unwrap();
        let one = plan(case(), "Timeline", spec).unwrap();
        assert_eq!(one, plan(case(), "Timeline", spec).unwrap());
        let other: Id = "case_019acd16-0000-7000-8000-000000000002".parse().unwrap();
        let two = plan(other, "Timeline", spec).unwrap();
        assert_ne!(one.1, two.1);
        assert_eq!(
            one.1.replace("0001", "0002"),
            two.1,
            "only the case ID differs"
        );
    }
}
