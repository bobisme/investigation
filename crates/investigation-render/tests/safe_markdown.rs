//! Hostile authored and captured text stays inert in every rendering context.
//!
//! Each escaper is checked against the checked-in corpus in
//! `tests/fixtures/hostile-text.json` plus generated long inputs. Output is
//! parsed back: YAML with a YAML parser, Markdown with a CommonMark parser
//! with tables, footnotes, strikethrough, task lists, math, wikilinks, GFM
//! callouts and metadata blocks enabled. Obsidian-only syntax the parser does
//! not model (highlights, comments, tags, block references, inline fields and
//! autolinked bare URLs) is checked on the source and on the decoded text.

use std::collections::BTreeSet;

use investigation_core::identity::Id;
use investigation_render::{
    PlanPath, VerifiedSnapshot, ViewPolicy, layout, render,
    text::{
        EXCERPT_LANGUAGE, display_text, fenced_block, inline_code, inline_text,
        is_invisible_format, table_code,
    },
    yaml::Frontmatter,
};
use investigation_store::{Store, Workspace};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use serde_json::{Value, json};

const TITLE_CHARS: usize = investigation_render::note::TITLE_CHARS;

/// The checked-in corpus plus long lines and long runs.
fn corpus() -> Vec<(String, String)> {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/hostile-text.json")).expect("corpus parses");
    let mut cases: Vec<(String, String)> = fixture["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| {
            (
                case["name"].as_str().unwrap().to_owned(),
                case["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let generated = [
        ("long-line", "word [[x]] ".repeat(2_000)),
        ("long-backtick-run", "`".repeat(3_000)),
        (
            "long-url",
            format!("https://{}.invalid/", "a".repeat(5_000)),
        ),
        ("long-bidi", format!("{}x", "\u{202e}".repeat(1_000))),
        ("long-brackets", "[[".repeat(3_000)),
        ("long-hashes", "#".repeat(3_000)),
        ("long-lines", "line\n".repeat(2_000)),
        // A URL that crosses the display-text bound must not survive truncation.
        (
            "url-at-bound",
            format!("{}https://example.invalid", "a ".repeat(56)),
        ),
    ];
    cases.extend(
        generated
            .into_iter()
            .map(|(name, text)| (name.to_owned(), text)),
    );
    cases
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_WIKILINKS
}

fn tag_name(tag: &Tag<'_>) -> String {
    match tag {
        Tag::Heading { level, .. } => format!("{level}"),
        Tag::Link {
            link_type,
            dest_url,
            ..
        } => format!("link {link_type:?} {dest_url}"),
        Tag::Image {
            link_type,
            dest_url,
            ..
        } => format!("image {link_type:?} {dest_url}"),
        Tag::CodeBlock(CodeBlockKind::Fenced(language)) => format!("fence {language}"),
        other => format!("{other:?}"),
    }
}

/// The parsed structure of `markdown`, with adjacent text merged.
fn shape(markdown: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for event in Parser::new_ext(markdown, options()) {
        match event {
            Event::Text(text) => match out.last_mut() {
                Some(last) if last.starts_with("text:") => last.push_str(&text),
                _ => out.push(format!("text:{text}")),
            },
            Event::Code(code) => out.push(format!("code:{code}")),
            Event::Start(tag) => out.push(format!("start {}", tag_name(&tag))),
            Event::End(tag) => out.push(format!("end {tag:?}")),
            other => out.push(format!("{other:?}")),
        }
    }
    out
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn is_address_local(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_')
}

/// A pattern a viewer would autolink in decoded text, if any.
fn autolinkable(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for pattern in ["//", "www."] {
        if lower.contains(pattern) {
            return Some(pattern.to_owned());
        }
    }
    for scheme in [
        "http",
        "https",
        "ftp",
        "mailto",
        "xmpp",
        "tel",
        "javascript",
        "data",
        "file",
        "obsidian",
    ] {
        for (at, _) in lower.match_indices(&format!("{scheme}:")) {
            let starts_word = !lower[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let followed = lower[at + scheme.len() + 1..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace());
            if starts_word && followed {
                return Some(format!("{scheme}:"));
            }
        }
    }
    let chars: Vec<char> = text.chars().collect();
    for (at, &c) in chars.iter().enumerate() {
        if c == '@'
            && at > 0
            && is_address_local(chars[at - 1])
            && chars.get(at + 1).is_some_and(char::is_ascii_alphanumeric)
        {
            return Some("address".to_owned());
        }
    }
    None
}

/// Whether `chars[at..]` starts an HTML character reference.
fn starts_reference(chars: &[char], at: usize) -> bool {
    let name = chars[at + 1..]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '#')
        .count();
    name > 0 && chars.get(at + 1 + name) == Some(&';')
}

/// A display label is inert by its characters alone, with no escapes.
fn assert_inert_label(name: &str, label: &str) {
    let chars: Vec<char> = label.chars().collect();
    assert_eq!(label.trim(), label, "{name}: {label:?}");
    for (at, &c) in chars.iter().enumerate() {
        assert!(
            !(c.is_control() || is_invisible_format(c)),
            "{name}: {c:?} in {label:?}"
        );
        assert!(!"[]|#`*\\$^~<>".contains(c), "{name}: {c:?} in {label:?}");
        let next = chars.get(at + 1).copied();
        for doubled in ['=', '%', ':'] {
            assert!(
                !(c == doubled && next == Some(doubled)),
                "{name}: {doubled}{doubled} in {label:?}"
            );
        }
        if c == '_' {
            let intraword = at > 0
                && chars[at - 1].is_alphanumeric()
                && next.is_some_and(char::is_alphanumeric);
            assert!(intraword, "{name}: flanking underscore in {label:?}");
        }
        if c == '&' {
            assert!(!starts_reference(&chars, at), "{name}: entity in {label:?}");
        }
    }
    assert_eq!(autolinkable(label), None, "{name}: {label:?}");
}

/// Every ASCII punctuation mark in prose is backslash-escaped.
fn assert_escaped_prose(name: &str, prose: &str) {
    let mut chars = prose.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let next = chars.next();
            assert!(
                next.is_some_and(|c| c.is_ascii_punctuation()),
                "{name}: dangling backslash in {prose:?}"
            );
        } else {
            assert!(
                !c.is_ascii_punctuation(),
                "{name}: unescaped {c:?} in {prose:?}"
            );
            assert!(
                !(c.is_control() || is_invisible_format(c)),
                "{name}: {c:?} in {prose:?}"
            );
        }
    }
}

fn unescape(prose: &str) -> String {
    let mut out = String::new();
    let mut chars = prose.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next().unwrap() } else { c });
    }
    out
}

/// What a code span or fenced block shows for `text`.
fn visible(text: &str, keep_lines: bool) -> String {
    let mut out = String::new();
    let text = if keep_lines {
        text.replace("\r\n", "\n")
    } else {
        text.to_owned()
    };
    for c in text.chars() {
        if keep_lines && matches!(c, '\n' | '\t') {
            out.push(c);
        } else if c.is_control() || is_invisible_format(c) || matches!(c, '\u{fffe}' | '\u{ffff}') {
            out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn display_text_is_inert_as_heading_link_label_table_cell_and_alias() {
    let target = PlanPath::new("Cases/case_x/Questions/q_x.md").unwrap();
    for (name, text) in corpus() {
        let Some(label) = display_text(&text, TITLE_CHARS) else {
            assert!(
                text.chars()
                    .all(|c| c.is_whitespace() || c.is_control() || is_invisible_format(c)),
                "{name}: only blank input has no label"
            );
            continue;
        };
        assert!(label.chars().count() <= TITLE_CHARS, "{name}");
        assert_inert_label(&name, &label);
        assert_eq!(display_text(&label, TITLE_CHARS).as_ref(), Some(&label));

        // A heading: one H1 whose decoded text is exactly the label bytes.
        assert_eq!(
            shape(&format!("# {label}\n")),
            strings(&["start h1", &format!("text:{label}"), "end Heading(H1)"]),
            "{name}"
        );

        // A wikilink label: one internal link to the validated target.
        let link = layout::wikilink(&target, &label);
        assert_eq!(link, format!("[[Cases/case_x/Questions/q_x|{label}]]"));
        assert_eq!(
            shape(&format!("- {link} (revision 1)\n")),
            strings(&[
                "start List(None)",
                "start Item",
                "start link WikiLink { has_pothole: true } Cases/case_x/Questions/q_x",
                &format!("text:{label}"),
                "end Link",
                "text: (revision 1)",
                "end Item",
                "end List(false)",
            ]),
            "{name}"
        );

        // A table cell: still exactly two cells in the row.
        let table = shape(&format!(
            "| A | B |\n|---|---|\n| {label} | {} |\n",
            link.replace('|', "\\|")
        ));
        let cells = table.iter().filter(|e| *e == "start TableCell").count();
        assert_eq!(cells, 4, "{name}: {table:?}");
        assert!(
            table.contains(&format!("text:{label}")),
            "{name}: {table:?}"
        );
        // The parser keeps the table pipe escape in the target; viewers drop it.
        assert!(
            table.contains(
                &"start link WikiLink { has_pothole: true } Cases/case_x/Questions/q_x\\"
                    .to_owned()
            ),
            "{name}: {table:?}"
        );

        // An alias: one flat YAML string, equal to the label.
        let mut front = Frontmatter::new();
        front.list("aliases", &[label.as_str()]);
        let block = front.finish();
        let inner = block
            .strip_prefix("---\n")
            .unwrap()
            .strip_suffix("---\n")
            .unwrap();
        let map: yaml_serde::Mapping = yaml_serde::from_str(inner).unwrap();
        assert_eq!(map.len(), 1, "{name}");
        assert_eq!(map["aliases"][0], label.as_str());
    }
}

#[test]
fn inline_text_is_inert_prose_and_a_single_table_cell() {
    for (name, text) in corpus() {
        let prose = inline_text(&text, 600);
        assert!(!prose.contains('\n') && !prose.contains('\r'), "{name}");
        assert_escaped_prose(&name, &prose);
        let decoded = unescape(&prose);
        assert_eq!(autolinkable(&decoded), None, "{name}: {decoded:?}");

        let expected = format!("Claim: {decoded}");
        assert_eq!(
            shape(&format!("- Claim: {prose}\n")),
            strings(&[
                "start List(None)",
                "start Item",
                &format!("text:{}", expected.trim_end()),
                "end Item",
                "end List(false)",
            ]),
            "{name}"
        );
        let table = shape(&format!("| Reason |\n|---|\n| {prose} |\n"));
        let cells = table.iter().filter(|e| *e == "start TableCell").count();
        assert_eq!(cells, 2, "{name}: {table:?}");
        if !decoded.is_empty() {
            assert!(
                table.contains(&format!("text:{decoded}")),
                "{name}: {table:?}"
            );
        }
    }
}

#[test]
fn code_spans_show_exact_text_on_a_line_and_in_a_table_cell() {
    for (name, text) in corpus() {
        let mut shown = visible(&text, false);
        // A leading Dataview prefix character is shown as an escape.
        let lead = shown.len() - shown.trim_start().len();
        if shown[lead..].starts_with('=') {
            shown.replace_range(lead..=lead, "\\u{3d}");
        } else if shown[lead..].starts_with("$=") {
            shown.replace_range(lead..=lead, "\\u{24}");
        }
        // An empty code span cannot exist; two spaces mark the empty value.
        let shown = if shown.is_empty() {
            "  ".to_owned()
        } else {
            shown
        };
        assert_eq!(
            shape(&format!("before {} after\n", inline_code(&text))),
            strings(&[
                "start Paragraph",
                "text:before ",
                &format!("code:{shown}"),
                "text: after",
                "end Paragraph",
            ]),
            "{name}"
        );
        let table = shape(&format!(
            "| Selector | Next |\n|---|---|\n| {} | x |\n",
            table_code(&text)
        ));
        let cells = table.iter().filter(|e| *e == "start TableCell").count();
        assert_eq!(cells, 4, "{name}: {table:?}");
        assert!(
            table.contains(&format!("code:{shown}")),
            "{name}: {table:?}"
        );
    }
}

#[test]
fn hostile_code_text_never_starts_a_dataview_inline_query() {
    let hostile = [
        "=this.file.name",
        "$=dv.el(\"p\", dv.current().file.name)",
        " = x",
        "\u{a0}$=x",
        "  =x",
        "`=x`",
        "\n=x",
    ];
    for text in hostile {
        for markdown in [
            format!("before {} after\n", inline_code(text)),
            format!("| A | B |\n|---|---|\n| {} | x |\n", table_code(text)),
        ] {
            let mut spans = 0;
            for event in Parser::new_ext(&markdown, options()) {
                if let Event::Code(code) = event {
                    spans += 1;
                    let code = code.trim();
                    assert!(
                        !code.starts_with('=') && !code.starts_with("$="),
                        "{text:?}: {code:?}"
                    );
                }
            }
            assert_eq!(spans, 1, "{text:?}: {markdown:?}");
        }
    }
}

#[test]
fn fenced_blocks_keep_excerpts_inside_one_text_block() {
    for (name, text) in corpus() {
        let body: String = visible(&text, true)
            .lines()
            .map(|line| format!("{line}\n"))
            .collect();
        let mut expected = vec![format!("start fence {EXCERPT_LANGUAGE}")];
        if !body.is_empty() {
            expected.push(format!("text:{body}"));
        }
        expected.extend(strings(&[
            "end CodeBlock",
            "start Paragraph",
            "text:after",
            "end Paragraph",
        ]));
        let block = fenced_block(&text, "");
        assert_eq!(shape(&format!("{block}\nafter\n")), expected, "{name}");

        // Inside a callout the block stays inside the quote.
        let quoted = shape(&format!(
            "> [!quote] Excerpt\n{}\nafter\n",
            fenced_block(&text, "> ")
        ));
        let fences: Vec<&String> = quoted
            .iter()
            .filter(|e| e.starts_with("start fence") || e.starts_with("start CodeBlock"))
            .collect();
        assert_eq!(
            fences,
            [&format!("start fence {EXCERPT_LANGUAGE}")],
            "{name}"
        );
        let tail: Vec<&String> = quoted.iter().rev().take(4).rev().collect();
        assert!(tail[0].starts_with("end BlockQuote"), "{name}: {quoted:?}");
        assert_eq!(
            tail[1..],
            [
                &"start Paragraph".to_owned(),
                &"text:after".to_owned(),
                &"end Paragraph".to_owned()
            ],
            "{name}: {quoted:?}"
        );
    }
}

#[test]
fn frontmatter_values_never_add_keys_or_end_the_block() {
    for (name, text) in corpus() {
        let mut front = Frontmatter::new();
        front
            .text("inv_title", &text)
            .list("aliases", &[text.as_str()]);
        let document = format!("{}# Title\n", front.finish());
        let parsed = shape(&document);
        assert_eq!(parsed[0], "start MetadataBlock(YamlStyle)", "{name}");
        let end = parsed
            .iter()
            .position(|e| e == "end MetadataBlock(YamlStyle)")
            .expect("metadata block ends");
        assert_eq!(
            parsed[end + 1..],
            strings(&["start h1", "text:Title", "end Heading(H1)"]),
            "{name}"
        );
        let yaml = parsed[1].strip_prefix("text:").unwrap();
        let map: yaml_serde::Mapping = yaml_serde::from_str(yaml).unwrap();
        assert_eq!(map.len(), 2, "{name}");
        assert_eq!(map["inv_title"], text.as_str(), "{name}");
        assert_eq!(map["aliases"][0], text.as_str(), "{name}");
    }
}

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Option<Id> {
    let input = json!({
        "schema_version":"inv.command.v0-alpha",
        "command_id":key,
        "case_id":case_id,
        "actor":"agent:render-test",
        "preconditions":[],
        "operations":[operation]
    });
    let receipt = store.record(&serde_json::to_vec(&input).unwrap()).ok()?;
    receipt.created.first().map(|created| created.id)
}

/// Unescaped Obsidian syntax in `source` outside code, if any.
fn obsidian_syntax(source: &str) -> Option<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c == '\\' {
            at += 2;
            continue;
        }
        let next = chars.get(at + 1).copied();
        let found = match c {
            '=' if next == Some('=') => Some("highlight"),
            '%' if next == Some('%') => Some("comment"),
            ':' if next == Some(':') => Some("inline field"),
            '$' => Some("math"),
            '<' => Some("html"),
            '#' if next.is_some_and(|n| !n.is_whitespace() && n != '#')
                && (at == 0 || chars[at - 1] != '#') =>
            {
                Some("tag")
            }
            '!' if next == Some('[') => Some("embed"),
            '&' if starts_reference(&chars, at) => Some("entity"),
            _ => None,
        };
        if let Some(found) = found {
            let context: String = chars[at.saturating_sub(20)..(at + 20).min(chars.len())]
                .iter()
                .collect();
            return Some(format!("{found} at {context:?}"));
        }
        at += 1;
    }
    None
}

const SECTIONS: &[&str] = &[
    "Summary",
    "Typed links",
    "Evidence selectors",
    "Next action or blocker",
    "Relies on",
    "Cited by",
    "Records",
    "Questions",
    "Projection health",
    "Who owns what",
    "Resume a case",
    "Cases",
    // Question notes (W08.04).
    "Question",
    "Addressed by",
    "Reviews and objections",
    // The case Overview (W08.05).
    "Opening summary",
    "Answer and caveat",
    "Evidence and assessments",
    "Sources",
    "Artifacts",
    "Reviews and retrospectives",
    "Reviews of the case",
    "Research",
    "Source matrix",
    "Contradicting accounts",
    "Open questions",
    "Resume",
    // Links to the case's table views (W08.07).
    "Views",
];

#[test]
fn hostile_titles_render_into_inert_vault_notes() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::init(&directory.path().join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let case_id = submit(
        &mut store,
        "case",
        None,
        json!({"op":"case.create", "payload":{
            "title":"Case ]] | [[Other]] <script>x</script> ![[e.png]] https://example.invalid #t",
            "mode":"record"}}),
    )
    .expect("case accepted");
    let mut accepted = 0;
    for (index, (_, text)) in corpus().into_iter().enumerate() {
        let operation =
            json!({"op":"question.create", "payload":{"wording":text, "importance":"normal"}});
        if submit(&mut store, &format!("q{index}"), Some(case_id), operation).is_some() {
            accepted += 1;
        }
    }
    assert!(
        accepted >= 100,
        "only {accepted} hostile questions were accepted"
    );

    let plan = render(
        &VerifiedSnapshot::read(&mut store).unwrap(),
        &ViewPolicy::default(),
    )
    .unwrap();
    let paths: BTreeSet<String> = plan
        .entries()
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    for entry in plan.entries() {
        let path = entry.path().to_string();
        let note = std::str::from_utf8(entry.content()).unwrap();
        assert!(!note.chars().any(is_invisible_format), "{path}");
        assert!(!note.chars().any(|c| c.is_control() && c != '\n'), "{path}");

        let mut headings = 0;
        let mut cells = 0;
        let mut columns = 0;
        let mut quotes = 0;
        let mut masked = String::new();
        let mut last = 0;
        let mut in_heading = None;
        let mut heading_text = String::new();
        for (event, range) in Parser::new_ext(note, options()).into_offset_iter() {
            match &event {
                Event::Html(_) | Event::InlineHtml(_) => panic!("HTML in {path}: {event:?}"),
                Event::InlineMath(_) | Event::DisplayMath(_) | Event::FootnoteReference(_) => {
                    panic!("{event:?} in {path}")
                }
                Event::TaskListMarker(_) | Event::Rule => panic!("{event:?} in {path}"),
                Event::Text(text) => {
                    assert_eq!(autolinkable(text), None, "{path}: {text:?}");
                    if in_heading.is_some() {
                        heading_text.push_str(text);
                    }
                }
                Event::Start(Tag::Image { .. }) => panic!("embed in {path}: {event:?}"),
                Event::Start(Tag::Link {
                    link_type: pulldown_cmark::LinkType::WikiLink { .. },
                    dest_url,
                    ..
                }) => assert!(
                    paths.contains(&format!("{}.md", dest_url.trim_end_matches('\\'))),
                    "{path}: link to {dest_url}"
                ),
                Event::Start(Tag::Link { .. }) => panic!("data link in {path}: {event:?}"),
                Event::Start(Tag::CodeBlock(kind)) => assert_eq!(
                    kind,
                    &CodeBlockKind::Fenced(EXCERPT_LANGUAGE.into()),
                    "{path}"
                ),
                Event::Start(
                    Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Superscript
                    | Tag::Subscript
                    | Tag::HtmlBlock
                    | Tag::FootnoteDefinition(_),
                ) => panic!("{event:?} in {path}"),
                // A Dataview inline query or script starts with `=` or `$=`.
                Event::Code(code) => {
                    let code = code.trim();
                    assert!(
                        !code.starts_with('=') && !code.starts_with("$="),
                        "{path}: inline query {code:?}"
                    );
                }
                Event::Start(Tag::TableCell) => cells += 1,
                Event::End(TagEnd::TableHead) => {
                    // A record note's tables are label and value; a view's
                    // table has its own columns, the same on every row.
                    if !path.contains("/Views/") {
                        assert_eq!(cells, 2, "{path}: a table row has two cells");
                    }
                    columns = cells;
                    cells = 0;
                }
                Event::End(TagEnd::TableRow) => {
                    assert_eq!(
                        cells, columns,
                        "{path}: a row has as many cells as its header"
                    );
                    cells = 0;
                }
                Event::Start(Tag::BlockQuote(_)) => quotes += 1,
                Event::Start(Tag::Heading { level, .. }) => {
                    in_heading = Some(*level);
                    heading_text.clear();
                }
                Event::End(TagEnd::Heading(level)) => {
                    if *level == pulldown_cmark::HeadingLevel::H1 {
                        headings += 1;
                    } else {
                        assert!(
                            SECTIONS.contains(&heading_text.as_str()),
                            "{path}: heading {heading_text:?}"
                        );
                    }
                    in_heading = None;
                }
                _ => {}
            }
            // Mask code and the metadata block before the source-level checks.
            let opaque = matches!(event, Event::Code(_))
                || matches!(
                    event,
                    Event::Start(Tag::MetadataBlock(_) | Tag::CodeBlock(_))
                );
            if opaque && range.start >= last {
                masked.push_str(&note[last..range.start]);
                masked.push_str(&" ".repeat(range.len()));
                last = range.end;
            }
        }
        masked.push_str(&note[last.min(note.len())..]);
        assert_eq!(headings, 1, "{path}: exactly one H1");
        assert_eq!(quotes, 1, "{path}: the banner is the only quote or callout");
        assert!(
            note.starts_with("---\n") || path == "Start Here.md",
            "{path}"
        );
        if let Some(found) = obsidian_syntax(&masked) {
            panic!("{path}: {found}");
        }
        for line in masked.lines() {
            // Callouts and block references come only from the templates.
            if let Some(at) = line.find("[!") {
                assert!(
                    line[at..].starts_with("[!warning] Tool-owned projection"),
                    "{path}: {line}"
                );
            }
            if let Some(at) = line.rfind(" ^") {
                let anchor = &line[at + 2..];
                assert!(
                    anchor.starts_with("inv-sel-") && !anchor.contains(' '),
                    "{path}: block reference in {line}"
                );
            }
        }
        if entry.projects().is_some() {
            let rest = note.strip_prefix("---\n").unwrap();
            let end = rest.find("\n---\n").unwrap();
            let map: yaml_serde::Mapping = yaml_serde::from_str(&rest[..=end]).unwrap();
            for (key, value) in &map {
                let key = key.as_str().unwrap();
                assert!(
                    key.starts_with("inv_") || key == "aliases" || key == "tags",
                    "{path}: {key}"
                );
                if key == "aliases" {
                    for alias in value.as_sequence().unwrap() {
                        assert_inert_label(&path, alias.as_str().unwrap());
                    }
                }
            }
        }
    }
}
