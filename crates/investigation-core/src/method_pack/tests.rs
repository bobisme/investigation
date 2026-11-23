use std::collections::BTreeSet;

use super::*;

const COMPLETE: &[u8] = include_bytes!("testdata/complete.toml");

const HEADER: &str = r#"schema_version = "inv.method-pack.v1"
id = "t"
version = "1.0.0"
title = "T"
summary = "S"
modes = ["explain", "measure"]
"#;

const CARD: &str = r#"
[[cards]]
name = "c"
title = "Card"
stage = "explain"
summary = "When it applies."
"#;

fn pack(top: &str, tables: &str) -> String {
    format!("{HEADER}{top}\n{tables}")
}

fn card(fields: &str, body: &str) -> String {
    format!("{CARD}{fields}\n[cards.body]\nwhen = \"w\"\nrecord = \"r\"\nskip = \"s\"\n{body}")
}

fn reject(text: &str) -> PackError {
    load(text.as_bytes()).expect_err("pack should be rejected")
}

fn assert_rejected(text: &str, kind: PackErrorKind, path: &str) {
    let error = reject(text);
    assert_eq!(
        (error.kind, error.path.as_str()),
        (kind, path),
        "{error:?} for\n{text}"
    );
}

#[test]
fn complete_fixture_loads_every_section() {
    let loaded = load(COMPLETE).unwrap();
    let pack = loaded.pack();
    assert_eq!(
        (loaded.id(), loaded.version()),
        ("fixture-complete", "1.2.3")
    );
    assert_eq!(pack.cards.len(), 2);
    let card = loaded.card("discriminate").unwrap();
    assert_eq!(card.triggers.len(), 3);
    assert_eq!(
        card.triggers[0].case_fact,
        Some(CaseFact::CompetingHypotheses)
    );
    assert_eq!(
        card.triggers[2].record_kind.map(RecordKind::kind),
        Some(Kind::Hypothesis)
    );
    assert!(card.body.record.contains('\n'));
    assert_eq!(pack.properties.len(), 3);
    assert_eq!(pack.observation_guidance.len(), 1);
    assert_eq!(pack.policies[1].input, PolicyInput::BlindspotReview);
    assert_eq!(pack.views[0].view, "record-table");
    assert_eq!(pack.reports[0].template, "case.summary");
    assert_eq!(loaded.byte_len(), COMPLETE.len());
    assert!(loaded.digest().as_str().starts_with("sha256:"));
}

#[test]
fn decoded_packs_match_the_published_shape() {
    let schema = serde_json::to_value(schema()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for bytes in [COMPLETE, GENERAL_PACK_SOURCE] {
        let value = serde_json::to_value(load(bytes).unwrap().pack()).unwrap();
        assert!(validator.is_valid(&value), "{value}");
    }
}

#[test]
fn builtin_general_pack_is_valid_and_neutral() {
    let general = builtin_general();
    assert_eq!(general.id(), "general");
    assert_eq!(general.pack().schema_version, SCHEMA_VERSION);
    assert_eq!(general.pack().modes.len(), 5);
    assert_eq!(general, builtin_general());
}

#[test]
fn same_bytes_always_yield_the_same_digest_and_value() {
    let first = load(COMPLETE).unwrap();
    let second = load(COMPLETE).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_vec(first.pack()).unwrap(),
        serde_json::to_vec(second.pack()).unwrap()
    );
    assert_eq!(
        first.digest().as_str(),
        format!("sha256:{:x}", Sha256::digest(COMPLETE))
    );

    let minimal = pack("", &card("", ""));
    let digest = load(minimal.as_bytes()).unwrap().digest().clone();
    assert_eq!(
        digest.as_str(),
        format!("sha256:{:x}", Sha256::digest(minimal.as_bytes()))
    );
    // The digest covers file bytes, not the parsed value.
    let spaced = minimal.replace("title = \"T\"", "title   =   \"T\"");
    let reformatted = load(spaced.as_bytes()).unwrap();
    assert_eq!(reformatted.pack(), load(minimal.as_bytes()).unwrap().pack());
    assert_ne!(reformatted.digest(), &digest);

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/method_pack/testdata/complete.toml");
    assert_eq!(load_file(&path).unwrap(), first);
    assert_eq!(load_file(&path).unwrap(), first);
}

#[test]
fn unknown_keys_are_rejected_with_their_path() {
    assert_rejected(
        &pack("colour = \"red\"", ""),
        PackErrorKind::InvalidField,
        "/colour",
    );
    assert_rejected(
        &pack("", &card("", "extra = \"x\"")),
        PackErrorKind::InvalidField,
        "/cards/0/body/extra",
    );
    assert_rejected(
        &pack(
            "",
            &card(
                "triggers = [{ gap_rule = \"conclusion.disputed\", weight = 2 }]",
                "",
            ),
        ),
        PackErrorKind::InvalidField,
        "/cards/0/triggers/0/weight",
    );
    assert_rejected(
        &pack(
            "",
            r#"[[properties]]
name = "p"
record_kinds = ["observation"]
description = "d"
schema = { type = "string", max_length = 8, pattern = "^(a+)+$" }"#,
        ),
        PackErrorKind::InvalidField,
        "/properties/0/schema/pattern",
    );
    assert_rejected(
        &pack(
            "",
            &card("triggers = [{ case_fact = \"looks_risky\" }]", ""),
        ),
        PackErrorKind::InvalidField,
        "/cards/0/triggers/0/case_fact",
    );
}

#[test]
fn executable_looking_fields_are_rejected_before_decoding() {
    for (top, tables, path) in [
        ("command = \"make test\"", String::new(), "/command"),
        ("include = \"other.toml\"", String::new(), "/include"),
        (
            "url = \"https://example.invalid/pack\"",
            String::new(),
            "/url",
        ),
        ("", card("hook = \"post-load\"", ""), "/cards/0/hook"),
        ("", card("on_select = \"x\"", ""), "/cards/0/on_select"),
        ("", card("", "Pre-Check = \"x\""), "/cards/0/body/Pre-Check"),
        (
            "",
            card("", "verify_script = \"x\""),
            "/cards/0/body/verify_script",
        ),
        (
            "",
            card("", "exec = [\"sh\", \"-c\", \"true\"]"),
            "/cards/0/body/exec",
        ),
        (
            "",
            r#"[[properties]]
name = "p"
record_kinds = ["observation"]
description = "d"
schema = { type = "object", "$ref" = "https://example.invalid/schema.json" }"#
                .to_owned(),
            "/properties/0/schema/$ref",
        ),
        (
            "[policy_hooks]\nbefore = \"x\"",
            String::new(),
            "/policy_hooks",
        ),
    ] {
        assert_rejected(&pack(top, &tables), PackErrorKind::ExecutableField, path);
    }
}

#[test]
fn oversize_text_and_files_are_rejected() {
    let long = "x".repeat(MAX_TEXT_BYTES + 1);
    assert_rejected(
        &pack(
            "",
            &format!("{CARD}\n[cards.body]\nwhen = \"{long}\"\nrecord = \"r\"\nskip = \"s\"\n"),
        ),
        PackErrorKind::TextTooLong,
        "/cards/0/body/when",
    );
    let section = "x".repeat(MAX_TEXT_BYTES);
    assert_rejected(
        &pack(
            "",
            &format!(
                "{CARD}\n[cards.body]\nwhen = \"{section}\"\nrecord = \"{section}\"\nskip = \"s\"\n"
            ),
        ),
        PackErrorKind::TextTooLong,
        "/cards/0/body",
    );
    assert_rejected(
        &pack(
            "",
            &card(
                &format!("title = \"{}\"", "t".repeat(MAX_LINE_BYTES + 1)),
                "",
            ),
        )
        .replace("title = \"Card\"\n", ""),
        PackErrorKind::TextTooLong,
        "/cards/0/title",
    );
    let oversize = format!("{}\n# {}", pack("", ""), "x".repeat(MAX_PACK_BYTES));
    assert_rejected(&oversize, PackErrorKind::TooLarge, "");
    let many = (0..=MAX_ENTRIES)
        .map(|index| card("", "").replace("name = \"c\"", &format!("name = \"c{index}\"")))
        .collect::<String>();
    assert_rejected(&pack("", &many), PackErrorKind::TooLarge, "/cards");
}

#[test]
fn deep_nesting_is_bounded() {
    let nested = format!("deep = {}1{}", "[".repeat(40), "]".repeat(40));
    let error = reject(&pack(&nested, ""));
    assert!(
        matches!(
            error.kind,
            PackErrorKind::TooDeep | PackErrorKind::InvalidToml
        ),
        "{error:?}"
    );
    let hostile = format!("deep = {}", "[".repeat(100_000));
    assert_eq!(reject(&pack(&hostile, "")).kind, PackErrorKind::InvalidToml);
    let nested = format!(
        "deep = {}1{}",
        "[".repeat(MAX_PACK_DEPTH + 1),
        "]".repeat(MAX_PACK_DEPTH + 1)
    );
    let error = reject(&pack(&nested, ""));
    assert_eq!(error.kind, PackErrorKind::TooDeep, "{error:?}");
    assert!(error.path.starts_with("/deep/0/0"));
}

#[test]
fn values_outside_strict_input_rules_are_rejected() {
    for (top, kind, path) in [
        ("x = 1.5", PackErrorKind::UnsupportedValue, "/x"),
        ("x = nan", PackErrorKind::UnsupportedValue, "/x"),
        ("x = inf", PackErrorKind::UnsupportedValue, "/x"),
        ("x = 1979-05-27", PackErrorKind::UnsupportedValue, "/x"),
        ("x = 9007199254740992", PackErrorKind::UnsafeNumber, "/x"),
    ] {
        assert_rejected(&pack(top, ""), kind, path);
    }
    let duplicate = reject(&format!("{HEADER}title = \"again\"\n"));
    assert_eq!(duplicate.kind, PackErrorKind::InvalidToml);
    assert_eq!(duplicate.line, Some(7));
    assert_eq!(reject("x = \"\u{0}").kind, PackErrorKind::InvalidToml);
    let error = load(&[0xff, 0xfe]).unwrap_err();
    assert_eq!(
        (error.kind, error.path.as_str()),
        (PackErrorKind::InvalidUtf8, "")
    );
}

#[test]
fn versions_are_checked_before_the_shape() {
    let missing = HEADER.replace("schema_version = \"inv.method-pack.v1\"\n", "");
    assert_rejected(&missing, PackErrorKind::InvalidField, "/schema_version");
    let future = HEADER.replace("v1", "v2") + "new_field = true\n";
    assert_rejected(
        &future,
        PackErrorKind::UnsupportedVersion,
        "/schema_version",
    );
    for version in ["1.0", "01.0.0", "1.0.0-rc1", "v1.0.0"] {
        let text = HEADER.replace("\"1.0.0\"", &format!("\"{version}\""));
        assert_rejected(&text, PackErrorKind::InvalidName, "/version");
    }
}

#[test]
fn references_outside_the_closed_vocabulary_are_rejected() {
    for (fields, kind, path) in [
        (
            "triggers = [{ gap_rule = \"custom.my_predicate\" }]",
            PackErrorKind::UnknownReference,
            "/cards/0/triggers/0/gap_rule",
        ),
        (
            "triggers = [{ gap_rule = \"conclusion.disputed\", case_fact = \"stalled\" }]",
            PackErrorKind::InvalidField,
            "/cards/0/triggers/0",
        ),
        (
            "triggers = [{}]",
            PackErrorKind::InvalidField,
            "/cards/0/triggers/0",
        ),
        (
            "triggers = [{ case_fact = \"stalled\" }, { case_fact = \"stalled\" }]",
            PackErrorKind::Duplicate,
            "/cards/0/triggers/1",
        ),
        (
            "operations = [\"hypothesis.create\", \"shell.run_anything\"]",
            PackErrorKind::UnknownReference,
            "/cards/0/operations/1",
        ),
        (
            // Any operation still reserved in the catalog; pick another when this one activates.
            "operations = [\"coverage.define\"]",
            PackErrorKind::UnknownReference,
            "/cards/0/operations/0",
        ),
        (
            "modes = [\"evaluate\"]",
            PackErrorKind::UnknownReference,
            "/cards/0/modes/0",
        ),
    ] {
        assert_rejected(&pack("", &card(fields, "")), kind, path);
    }
    assert_rejected(
        &pack(
            "",
            &card("", "").replace("stage = \"explain\"", "stage = \"deploy\""),
        ),
        PackErrorKind::UnknownReference,
        "/cards/0/stage",
    );
    for gate in [
        "observation.add",
        "run.record",
        "source.add",
        "annotation.add",
    ] {
        let policy = format!(
            "[[policies]]\nname = \"p\"\ninput = \"distinct_actor\"\ngate = \"{gate}\"\nrationale = \"r\"\n"
        );
        assert_rejected(
            &pack("", &policy),
            PackErrorKind::UnknownReference,
            "/policies/0/gate",
        );
    }
    let fieldless = "[[policies]]\nname = \"p\"\ninput = \"required_fields\"\ngate = \"decision.accept\"\nrationale = \"r\"\n";
    assert_rejected(
        &pack("", fieldless),
        PackErrorKind::InvalidField,
        "/policies/0/fields",
    );
    let stray = "[[policies]]\nname = \"p\"\ninput = \"distinct_actor\"\ngate = \"decision.accept\"\nfields = [\"x\"]\nrationale = \"r\"\n";
    assert_rejected(
        &pack("", stray),
        PackErrorKind::InvalidField,
        "/policies/0/fields",
    );
}

#[test]
fn text_is_plain_prose_and_names_are_identifiers() {
    for (body, path) in [
        (
            "mistake = \"Render {{ case.title }}\"",
            "/cards/0/body/mistake",
        ),
        ("mistake = \"${HOME}\"", "/cards/0/body/mistake"),
        ("mistake = \"<SCRIPT>x</script>\"", "/cards/0/body/mistake"),
        ("mistake = \"bell \\u0007\"", "/cards/0/body/mistake"),
        ("mistake = \"   \"", "/cards/0/body/mistake"),
    ] {
        let error = reject(&pack("", &card("", body)));
        assert_eq!(
            (
                error.path.as_str(),
                matches!(error.kind, PackErrorKind::InvalidText)
            ),
            (path, true),
            "{error:?}"
        );
    }
    assert_rejected(
        &pack(
            "",
            &card("", "").replace(
                "summary = \"When it applies.\"",
                "summary = \"two\\nlines\"",
            ),
        ),
        PackErrorKind::InvalidText,
        "/cards/0/summary",
    );
    for id in [
        "../escape",
        "https://example.invalid",
        "Upper",
        "a--b",
        "a-",
        "",
    ] {
        let text = HEADER.replace("id = \"t\"", &format!("id = \"{id}\""));
        assert_rejected(&text, PackErrorKind::InvalidName, "/id");
    }
    let view = "[[views]]\nname = \"v\"\ntitle = \"V\"\nview = \"file:///etc/passwd\"\n";
    assert_rejected(&pack("", view), PackErrorKind::InvalidName, "/views/0/view");
    let report = "[[reports]]\nname = \"r\"\ntitle = \"R\"\ntemplate = \"../../template.md\"\n";
    assert_rejected(
        &pack("", report),
        PackErrorKind::InvalidName,
        "/reports/0/template",
    );
}

#[test]
fn duplicate_entries_are_rejected() {
    let cards = format!("{}{}", card("", ""), card("", ""));
    assert_rejected(&pack("", &cards), PackErrorKind::Duplicate, "/cards/1");
    let modes = HEADER.replace("[\"explain\", \"measure\"]", "[\"explain\", \"explain\"]");
    assert_rejected(&modes, PackErrorKind::Duplicate, "/modes/1");
    let empty = HEADER.replace("[\"explain\", \"measure\"]", "[]");
    assert_rejected(&empty, PackErrorKind::InvalidField, "/modes");
}

#[test]
fn property_schemas_use_a_closed_bounded_subset() {
    let property = |schema: &str| {
        pack(
            "",
            &format!(
                "[[properties]]\nname = \"p\"\nrecord_kinds = [\"observation\"]\ndescription = \"d\"\nschema = {schema}\n"
            ),
        )
    };
    for (schema, kind, path) in [
        (
            "{ type = \"string\" }",
            PackErrorKind::InvalidSchema,
            "/properties/0/schema/max_length",
        ),
        (
            "{ type = \"string\", max_length = 100000 }",
            PackErrorKind::InvalidSchema,
            "/properties/0/schema/max_length",
        ),
        (
            "{ type = \"integer\", items = { type = \"boolean\" } }",
            PackErrorKind::InvalidSchema,
            "/properties/0/schema/items",
        ),
        (
            "{ type = \"integer\", minimum = 5, maximum = 1 }",
            PackErrorKind::InvalidSchema,
            "/properties/0/schema/minimum",
        ),
        (
            "{ type = \"array\", max_items = 4 }",
            PackErrorKind::InvalidSchema,
            "/properties/0/schema/items",
        ),
        (
            "{ type = \"array\", max_items = 4, items = { type = \"string\" } }",
            PackErrorKind::InvalidSchema,
            "/properties/0/schema/items/max_length",
        ),
        (
            "{ type = \"string\", max_length = 3, enum = [\"long\"] }",
            PackErrorKind::InvalidSchema,
            "/properties/0/schema/enum/0",
        ),
        (
            "{ type = \"object\", fields = [{ name = \"a\", schema = { type = \"boolean\" } }], required = [\"b\"] }",
            PackErrorKind::UnknownReference,
            "/properties/0/schema/required/0",
        ),
        (
            "{ type = \"object\", fields = [{ name = \"Bad-Name\", schema = { type = \"boolean\" } }] }",
            PackErrorKind::InvalidName,
            "/properties/0/schema/fields/0/name",
        ),
        (
            "{ type = \"regex\" }",
            PackErrorKind::InvalidField,
            "/properties/0/schema/type",
        ),
    ] {
        assert_rejected(&property(schema), kind, path);
    }
    let unknown_kind = pack(
        "",
        "[[properties]]\nname = \"p\"\nrecord_kinds = [\"transaction\"]\ndescription = \"d\"\nschema = { type = \"boolean\" }\n",
    );
    assert_rejected(
        &unknown_kind,
        PackErrorKind::InvalidField,
        "/properties/0/record_kinds/0",
    );
}

#[test]
fn errors_never_echo_pack_content() {
    let secret = "do-not-echo-this-value";
    for text in [
        pack(&format!("id2 = \"{secret}\""), ""),
        pack("", &card(&format!("operations = [\"{secret}\"]"), "")),
        format!("{HEADER}{secret}"),
    ] {
        let error = reject(&text);
        let failure = Failure::from(error.clone());
        let rendered = format!("{error} {}", serde_json::to_string(&failure).unwrap());
        assert!(!rendered.contains("echo-this-value"), "{rendered}");
    }
}

/// Packs may only reference rules the core evaluates. Keep the list in step
/// with the rule IDs that `gap_rules` can emit.
#[test]
fn gap_rule_triggers_match_the_built_in_rules() {
    let source = include_str!("../gap_rules.rs");
    let rules_source = source.split("#[cfg(test)]").next().unwrap();
    let catalog = standard_catalog();
    let chunks: Vec<&str> = rules_source.split('"').collect();
    // Odd chunks are string literals; skip suggested operation names.
    let emitted: BTreeSet<&str> = chunks
        .iter()
        .enumerate()
        .skip(1)
        .step_by(2)
        .filter(|(index, _)| !chunks[index - 1].ends_with("Some("))
        .map(|(_, literal)| *literal)
        .filter(|literal| {
            let mut parts = literal.splitn(2, '.');
            let (Some(head), Some(tail)) = (parts.next(), parts.next()) else {
                return false;
            };
            !head.is_empty()
                && head.bytes().all(|b| b.is_ascii_lowercase())
                && !tail.is_empty()
                && tail.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                && !tail.ends_with('_')
                && head != "inv"
                && catalog.get(literal).is_none()
        })
        .collect();
    let declared: BTreeSet<&str> = GAP_RULE_TRIGGERS.iter().copied().collect();
    assert_eq!(declared, emitted);
    assert!(GAP_RULE_TRIGGERS.is_sorted());
}

#[test]
fn policy_gates_are_promotion_operations_only() {
    let catalog = standard_catalog();
    for gate in POLICY_GATES {
        let operation = catalog.get(gate).expect("gate is a catalog operation");
        assert_eq!(operation.status(), Status::Active, "{gate}");
        assert!(
            !matches!(
                operation.info().stage,
                "observe" | "collect" | "record" | "start"
            ),
            "{gate} records evidence"
        );
    }
}

/// The loader is inert: the module neither spawns processes, opens network
/// connections nor writes. `load` takes bytes; `load_file` opens read-only.
#[test]
fn loader_source_contains_no_spawn_network_or_write_capability() {
    let source = include_str!("../method_pack.rs");
    for forbidden in [
        "std::process",
        "Command::new",
        "std::net",
        "TcpStream",
        "UdpSocket",
        "OpenOptions",
        "File::create",
        "fs::write",
        "fs::remove",
        "fs::rename",
        "fs::copy",
        "create_dir",
        "set_permissions",
        "std::env",
        "unsafe {",
        "unsafe fn",
        "libloading",
        "dlopen",
    ] {
        assert!(
            !source.contains(forbidden),
            "method_pack.rs mentions {forbidden}"
        );
    }
    assert_eq!(source.matches("std::fs::").count(), 1, "one read-only open");
    assert!(source.contains("std::fs::File::open(path)"));
}

#[test]
fn loading_a_file_leaves_its_directory_unchanged() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/method_pack/testdata");
    let listing = || {
        let mut entries: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                (
                    entry.file_name(),
                    metadata.len(),
                    metadata.modified().unwrap(),
                )
            })
            .collect();
        entries.sort();
        entries
    };
    let before = listing();
    for _ in 0..3 {
        load_file(&directory.join("complete.toml")).unwrap();
    }
    assert!(load_file(&directory.join("missing.toml")).is_err());
    assert_eq!(listing(), before);
}

#[test]
fn rejections_map_to_invalid_input_failures_with_paths() {
    let failure = Failure::from(reject(&pack("", &card("hook = \"x\"", ""))));
    let value = serde_json::to_value(&failure).unwrap();
    assert_eq!(value["error"]["code"], "INVALID_INPUT");
    assert_eq!(value["error"]["path"], "/cards/0/hook");
}
