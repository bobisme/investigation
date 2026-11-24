use super::*;
use crate::PlanPath;

/// Export planning must stay pure, like the vault planner: no filesystem,
/// clock, randomness or hash-order iteration in the modules that build an
/// export's bytes.
#[test]
fn export_planner_sources_have_no_filesystem_clock_or_randomness() {
    let sources = [
        ("export/mod.rs", include_str!("mod.rs")),
        ("export/inclusion.rs", include_str!("inclusion.rs")),
        ("export/identifiers.rs", include_str!("identifiers.rs")),
        ("export/manifest.rs", include_str!("manifest.rs")),
        ("export/case_report.rs", include_str!("case_report.rs")),
        (
            "export/decision_record.rs",
            include_str!("decision_record.rs"),
        ),
        ("export/archive.rs", include_str!("archive.rs")),
        ("export/retained.rs", include_str!("retained.rs")),
    ];
    let forbidden = [
        ["std", "::fs"].concat(),
        ["std", "::env"].concat(),
        ["std", "::time"].concat(),
        ["Hash", "Map"].concat(),
        ["Hash", "Set"].concat(),
        ["rust", "ix"].concat(),
        ["System", "Time"].concat(),
        ["Instant", "::"].concat(),
        ["rand", "::"].concat(),
        ["crate::", "publish"].concat(),
        ["Store", "<"].concat(),
        ["Workspace"].concat(),
    ];
    for (name, source) in sources {
        let body = source.split("#[cfg(test)]").next().unwrap();
        for term in &forbidden {
            assert!(
                !body.contains(term.as_str()),
                "{name} must stay pure but mentions {term}"
            );
        }
    }
}

/// No export path makes a network call, starts a process (a viewer, a
/// model client or anything else) or names a model service.
#[test]
fn no_export_source_reaches_the_network_a_process_or_a_model() {
    let sources = [
        ("export/mod.rs", include_str!("mod.rs")),
        ("export/inclusion.rs", include_str!("inclusion.rs")),
        ("export/identifiers.rs", include_str!("identifiers.rs")),
        ("export/manifest.rs", include_str!("manifest.rs")),
        ("export/case_report.rs", include_str!("case_report.rs")),
        ("export/snapshot.rs", include_str!("snapshot.rs")),
        ("export/write.rs", include_str!("write.rs")),
        ("export/archive.rs", include_str!("archive.rs")),
        ("export/retained.rs", include_str!("retained.rs")),
        ("export/source.rs", include_str!("source.rs")),
        ("export/verify.rs", include_str!("verify.rs")),
    ];
    let forbidden = [
        ["std", "::net"].concat(),
        ["Tcp", "Stream"].concat(),
        ["Udp", "Socket"].concat(),
        ["std", "::process"].concat(),
        ["Command", "::new"].concat(),
        ["http", "://"].concat(),
        ["https", "://"].concat(),
    ];
    for (name, source) in sources {
        let body = source.split("#[cfg(test)]").next().unwrap();
        for term in &forbidden {
            assert!(!body.contains(term.as_str()), "{name} mentions {term}");
        }
    }
}

#[test]
fn the_registry_lists_every_section_15_4_kind_once() {
    assert_eq!(
        kind_names(),
        [
            "handoff",
            "case-report",
            "analysis-report",
            "decision-record",
            "method-retrospective",
            "review-vault",
            "workspace-archive",
            "producer-bundle",
        ]
    );
    assert_eq!(
        available_kinds(),
        ["case-report", "decision-record", "workspace-archive"]
    );
    assert_eq!(
        kind("workspace-archive").map(KindSpec::scope),
        Some(Scope::All)
    );
    assert_eq!(kind("case-report").map(KindSpec::scope), Some(Scope::Case));
    assert!(kind("case-report").is_some_and(KindSpec::available));
    assert!(kind("handoff").is_some_and(|spec| !spec.available()));
    assert!(kind("vault").is_none());
    let resumable: Vec<_> = KINDS
        .iter()
        .filter(|spec| spec.product.resumable())
        .map(|spec| spec.name)
        .collect();
    assert_eq!(resumable, ["workspace-archive"]);
    for spec in KINDS {
        assert!(!spec.purpose.is_empty() && !spec.contains.is_empty());
        assert!(
            PlanPath::new(spec.name).is_ok(),
            "{} is a safe name",
            spec.name
        );
    }
}

#[test]
fn the_manifest_schema_is_closed_and_requires_every_field() {
    let schema = serde_json::to_value(manifest_schema()).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for field in [
        "schema_version",
        "export_id",
        "kind",
        "snapshot",
        "versions",
        "policy",
        "completeness",
        "sources",
        "supersession",
        "files",
        "statements",
    ] {
        assert!(required.contains(&field), "{field} is required");
    }
}

#[test]
fn an_unavailable_kind_is_refused_by_name() {
    let error = ExportError::KindNotAvailable { kind: "handoff" };
    let failure = error.to_failure();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert!(failure.error.message.contains("handoff"));
    assert!(failure.error.message.contains("case-report"));
}

#[test]
fn retained_bytes_are_admitted_only_under_their_own_digest() {
    let bytes = b"retained bytes".to_vec();
    let digest = hash_bytes(&bytes);
    let mut retained = Retained::none();
    let other = hash_bytes(b"other bytes");
    let refused = retained.insert(other.clone(), bytes.clone()).unwrap_err();
    assert_eq!(refused.expected, other);
    assert_eq!(refused.actual, digest);
    assert!(retained.is_empty());
    retained.insert(digest.clone(), bytes.clone()).unwrap();
    assert_eq!(retained.get(&digest), Some(bytes.as_slice()));
    assert_eq!(retained.get(&other), None);
}

#[test]
fn a_kind_that_covers_every_case_is_refused_by_the_case_planner() {
    let spec = kind("workspace-archive").unwrap();
    assert!(spec.available());
    assert_eq!(spec.product, Product::Archive);
    assert!(matches!(
        ExportError::AllCases { kind: spec.name }
            .to_failure()
            .error
            .code,
        investigation_core::wire::ErrorCode::InvalidInput
    ));
}
