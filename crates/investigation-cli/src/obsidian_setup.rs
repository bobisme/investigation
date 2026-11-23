//! `obsidian setup`: preview, and only on request apply, a few named native
//! viewer settings in a vault's configuration folder.
//!
//! Preview is the default and writes nothing. `--apply` changes only the
//! selected keys of one file, `app.json`, by splicing their values into the
//! file's own text, so every other byte stays as it was; every other file
//! in the configuration folder is never opened. The command creates no
//! folder, never follows a symbolic link ([`OwnedFile`]), and never touches
//! community plugin lists, plugin folders, themes, snippets or hotkeys.
//! Nothing here installs, enables or disables a plugin.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use investigation_core::{
    PROGRAM_NAME,
    wire::{ErrorCode, Failure},
};
use investigation_store::{
    journal::hash_bytes,
    owned_file::{MAX_OWNED_FILE_BYTES, OwnedFile, OwnedFileError},
};
use serde_json::{Map, Value, json};

use crate::{Output, display_argv, integration::unified_diff};

pub(crate) const AFTER_HELP: &str = "\
Preview or apply a few named native viewer settings in a vault you name.
Only setup exists; it never installs, enables or disables a plugin, and it
leaves Restricted Mode, plugin lists, themes, snippets and hotkeys alone.
Example: investigation obsidian setup --vault /home/me/Notes
Next: investigation obsidian setup --vault /home/me/Notes --format json";

const SETUP_HELP: &str = "\
Show the exact diff of the named settings against the vault's app.json, and
write nothing. --apply writes only the selected keys of that one file, leaves
every other byte of it as it was, creates no folder, and never follows a link.
The configuration folder is .obsidian unless one hidden folder of the vault
holds an app.json, or --config-dir names it. --setting limits the keys.
Examples:
  investigation obsidian setup --vault /home/me/Notes
  investigation obsidian setup --vault /home/me/Notes --setting useMarkdownLinks --apply
Next: investigation obsidian setup --vault /home/me/Notes --apply";

/// The one file whose keys setup may change.
pub(crate) const SETTINGS_FILE: &str = "app.json";
/// The configuration folder name used unless another is detected or named.
pub(crate) const DEFAULT_CONFIG_DIR: &str = ".obsidian";

/// One native setting setup can propose: a key of `app.json`, the value
/// proposed, and why the generated vault is easier to use with it.
pub(crate) struct Setting {
    pub(crate) key: &'static str,
    /// The value as JSON text.
    pub(crate) value: &'static str,
    pub(crate) reason: &'static str,
}

/// Every setting setup can propose. `obsidian/README.md` lists the same rows;
/// a test keeps the two equal.
pub(crate) const SETTINGS: &[Setting] = &[
    Setting {
        key: "useMarkdownLinks",
        value: "false",
        reason: "Generated notes link with wikilinks; links you create then use the same form.",
    },
    Setting {
        key: "newLinkFormat",
        value: "\"absolute\"",
        reason: "Generated links name the path from the vault root, so a link you create cannot resolve to a generated note of the same name.",
    },
];

/// What setup never reads, writes or changes. Listed in every response.
pub(crate) const NEVER_TOUCHED: &[&str] = &[
    "community-plugins.json",
    "plugins/",
    "themes/",
    "snippets/",
    "hotkeys.json",
    "workspace.json",
    "every other key of app.json",
    "Restricted Mode",
];

#[derive(clap::Subcommand)]
pub(crate) enum ObsidianCommand {
    /// Show, and with --apply write, named native viewer settings in a vault.
    #[command(after_help = SETUP_HELP)]
    Setup(SetupArgs),
}

#[derive(clap::Args)]
pub(crate) struct SetupArgs {
    /// The vault folder; an existing folder, never a link.
    #[arg(long, value_name = "PATH")]
    vault: PathBuf,
    /// The vault's configuration folder name, when it is not .obsidian and not detected.
    #[arg(long, value_name = "NAME")]
    config_dir: Option<String>,
    /// Limit setup to this key; repeat for several. Default: every proposed key.
    #[arg(long = "setting", value_name = "KEY")]
    settings: Vec<String>,
    /// Write the previewed change; without it nothing is written.
    #[arg(long)]
    apply: bool,
}

fn refusal(reason: &str, flag: &str, message: impl AsRef<str>) -> Failure {
    let mut failure = Failure::new(
        ErrorCode::InvalidInput,
        format!("{reason}: {} Nothing was written.", message.as_ref()),
    )
    .at(flag);
    failure.error.expected = Some(json!({ "reason": reason }));
    failure
}

fn file_failure(error: OwnedFileError) -> Failure {
    let (reason, message) = match &error {
        OwnedFileError::InvalidPath => ("INVALID_PATH", "The path does not name a file.".to_owned()),
        OwnedFileError::Symlink => (
            "SYMLINK_REFUSED",
            "The vault path, or a folder it names, is a symbolic link; setup never writes through a link. Name the real location.".to_owned(),
        ),
        OwnedFileError::DirectoryMissing => (
            "CONFIG_DIR_MISSING",
            "The vault's configuration folder does not exist. Setup creates no folder: open the vault in the viewer once, or name the folder with --config-dir.".to_owned(),
        ),
        OwnedFileError::NotRegular => (
            "NOT_REGULAR_FILE",
            "app.json is not a regular file.".to_owned(),
        ),
        OwnedFileError::MultipleLinks => (
            "MULTIPLE_LINKS",
            "app.json has more than one hard link; replacing it would leave the other names stale.".to_owned(),
        ),
        OwnedFileError::TooLarge { size, limit } => (
            "FILE_TOO_LARGE",
            format!("app.json holds {size} bytes; the limit is {limit}."),
        ),
        OwnedFileError::Unsupported => (
            "OPENAT2_UNSUPPORTED",
            "This kernel lacks openat2, so the path cannot be opened without following links.".to_owned(),
        ),
        OwnedFileError::Changed => {
            let mut failure = Failure::new(
                ErrorCode::IoFailure,
                "FILE_CHANGED: app.json changed while it was being updated. Nothing was written; run the command again.",
            )
            .at("--vault");
            failure.error.expected = Some(json!({ "reason": "FILE_CHANGED" }));
            return failure;
        }
        OwnedFileError::Io(io) => {
            return Failure::new(ErrorCode::IoFailure, format!("Change app.json: {io}."))
                .at("--vault");
        }
    };
    refusal(reason, "--vault", message)
}

fn valid_config_dir(name: &str) -> bool {
    name.len() > 1
        && name.starts_with('.')
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && Path::new(name).components().count() == 1
}

/// How the configuration folder was chosen.
#[derive(Clone, Copy)]
enum Source {
    Flag,
    Default,
    Detected,
}

impl Source {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Default => "default",
            Self::Detected => "detected",
        }
    }
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

fn resolve_config_dir(vault: &Path, flag: Option<&str>) -> Result<(String, Source), Failure> {
    if let Some(name) = flag {
        if !valid_config_dir(name) {
            return Err(refusal(
                "INVALID_CONFIG_DIR",
                "--config-dir",
                "The name must be one hidden folder name, such as .obsidian, with no separators.",
            ));
        }
        return Ok((name.to_owned(), Source::Flag));
    }
    let default = vault.join(DEFAULT_CONFIG_DIR);
    if is_real_dir(&default) {
        return Ok((DEFAULT_CONFIG_DIR.to_owned(), Source::Default));
    }
    if std::fs::symlink_metadata(&default).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(file_failure(OwnedFileError::Symlink));
    }
    let entries = std::fs::read_dir(vault).map_err(|error| {
        Failure::new(
            ErrorCode::IoFailure,
            format!("Read the vault folder: {error}."),
        )
        .at("--vault")
    })?;
    let mut found = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if valid_config_dir(&name)
            && is_real_dir(&entry.path())
            && std::fs::symlink_metadata(entry.path().join(SETTINGS_FILE))
                .is_ok_and(|meta| meta.is_file())
        {
            found.insert(name);
        }
    }
    let mut found = found.into_iter();
    match (found.next(), found.next()) {
        (Some(name), None) => Ok((name, Source::Detected)),
        (Some(first), Some(second)) => Err(refusal(
            "CONFIG_DIR_AMBIGUOUS",
            "--config-dir",
            format!(
                "Hidden folders {first} and {second} (and possibly more) each hold an app.json. Name the right one with --config-dir."
            ),
        )),
        _ => Err(refusal(
            "CONFIG_DIR_MISSING",
            "--vault",
            "The vault has no .obsidian folder and no hidden folder holding an app.json. Setup creates no folder: open the vault in the viewer once, or name the folder with --config-dir.",
        )),
    }
}

fn selected(names: &[String]) -> Result<Vec<&'static Setting>, Failure> {
    if names.is_empty() {
        return Ok(SETTINGS.iter().collect());
    }
    let mut chosen = Vec::new();
    for name in names {
        let Some(setting) = SETTINGS.iter().find(|setting| setting.key == name) else {
            let known: Vec<&str> = SETTINGS.iter().map(|setting| setting.key).collect();
            return Err(refusal(
                "UNKNOWN_SETTING",
                "--setting",
                format!(
                    "`{name}` is not a setting setup can propose; the keys are {}.",
                    known.join(", ")
                ),
            ));
        };
        if !chosen.iter().any(|seen: &&Setting| seen.key == setting.key) {
            chosen.push(setting);
        }
    }
    Ok(chosen)
}

fn proposed(setting: &Setting) -> Value {
    serde_json::from_str(setting.value).expect("a setting value is valid JSON")
}

/// One top-level member of a JSON object, located in the text.
struct Entry {
    key: String,
    value_start: usize,
    value_end: usize,
    /// Leading whitespace of the member's line, when the line holds only it.
    indent: Option<String>,
}

struct Scan {
    /// Index just after the opening brace.
    open: usize,
    /// Index of the closing brace.
    close: usize,
    entries: Vec<Entry>,
}

const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && is_space(bytes[at]) {
        at += 1;
    }
    at
}

/// Index just past the string that starts at `at`.
fn skip_string(bytes: &[u8], at: usize) -> usize {
    let mut index = at + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// Index just past the JSON value that starts at `at`.
fn skip_value(bytes: &[u8], at: usize) -> usize {
    match bytes[at] {
        b'"' => skip_string(bytes, at),
        b'{' | b'[' => {
            let mut depth = 0_usize;
            let mut index = at;
            while index < bytes.len() {
                match bytes[index] {
                    b'"' => {
                        index = skip_string(bytes, index);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return index + 1;
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            bytes.len()
        }
        _ => {
            let mut index = at;
            while index < bytes.len()
                && !is_space(bytes[index])
                && !matches!(bytes[index], b',' | b'}')
            {
                index += 1;
            }
            index
        }
    }
}

/// Locate the members of the top-level object in `text`, which the caller has
/// already parsed as one JSON object.
fn scan_object(text: &str) -> Scan {
    let bytes = text.as_bytes();
    let open = skip_space(bytes, 0) + 1;
    let mut entries = Vec::new();
    let mut at = skip_space(bytes, open);
    while at < bytes.len() && bytes[at] != b'}' {
        let key_end = skip_string(bytes, at);
        let key: String = serde_json::from_str(&text[at..key_end]).unwrap_or_default();
        let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
        let before = &text[line_start..at];
        let indent = (line_start > 0 && before.bytes().all(|b| matches!(b, b' ' | b'\t')))
            .then(|| before.to_owned());
        let colon = skip_space(bytes, key_end);
        let value_start = skip_space(bytes, colon + 1);
        let value_end = skip_value(bytes, value_start);
        entries.push(Entry {
            key,
            value_start,
            value_end,
            indent,
        });
        at = skip_space(bytes, value_end);
        if bytes.get(at) == Some(&b',') {
            at = skip_space(bytes, at + 1);
        }
    }
    Scan {
        open,
        close: at,
        entries,
    }
}

fn member(key: &str, value: &Value) -> String {
    format!(
        "{}: {}",
        serde_json::to_string(key).expect("a key serializes"),
        serde_json::to_string(value).expect("a value serializes")
    )
}

/// The text of `app.json` after the chosen keys are set. Only the chosen
/// keys' values and, for a key not yet present, one new member each are
/// touched; every other byte is the original's.
fn splice(before: Option<&str>, chosen: &[&Setting]) -> Result<String, Failure> {
    let fresh = || {
        let body: Vec<String> = chosen
            .iter()
            .map(|setting| format!("  {}", member(setting.key, &proposed(setting))))
            .collect();
        format!("{{\n{}\n}}\n", body.join(",\n"))
    };
    let Some(text) = before.filter(|text| !text.trim().is_empty()) else {
        return Ok(fresh());
    };
    let invalid = |detail: String| {
        refusal(
            "APP_JSON_INVALID",
            "--vault",
            format!("app.json is not a JSON object ({detail}), so it is left alone."),
        )
    };
    let parsed: Value = serde_json::from_str(text).map_err(|error| invalid(error.to_string()))?;
    let Value::Object(original) = &parsed else {
        return Err(invalid("its top level is not an object".to_owned()));
    };
    let scan = scan_object(text);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut added: Vec<String> = Vec::new();
    for setting in chosen {
        let value = proposed(setting);
        let places: Vec<&Entry> = scan
            .entries
            .iter()
            .filter(|entry| entry.key == setting.key)
            .collect();
        match places.as_slice() {
            [] => added.push(member(setting.key, &value)),
            [entry] => {
                if original.get(setting.key) != Some(&value) {
                    edits.push((
                        entry.value_start,
                        entry.value_end,
                        serde_json::to_string(&value).expect("a value serializes"),
                    ));
                }
            }
            _ => {
                return Err(refusal(
                    "APP_JSON_DUPLICATE_KEY",
                    "--vault",
                    format!(
                        "app.json names `{}` more than once; fix it by hand.",
                        setting.key
                    ),
                ));
            }
        }
    }
    if !added.is_empty() {
        match scan.entries.last() {
            Some(last) => {
                let separator = last
                    .indent
                    .as_ref()
                    .map_or_else(|| " ".to_owned(), |indent| format!("\n{indent}"));
                let insert: String = added
                    .iter()
                    .map(|member| format!(",{separator}{member}"))
                    .collect();
                edits.push((last.value_end, last.value_end, insert));
            }
            None => {
                let body: Vec<String> = added.iter().map(|member| format!("  {member}")).collect();
                edits.push((scan.open, scan.close, format!("\n{}\n", body.join(",\n"))));
            }
        }
    }
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.0));
    let mut after = text.to_owned();
    for (start, end, replacement) in edits {
        after.replace_range(start..end, &replacement);
    }
    check_only_named_keys_changed(original, &after, chosen)?;
    Ok(after)
}

/// Defence in depth: the spliced text must parse, hold every chosen key at
/// its proposed value, and hold every other key at its original value.
fn check_only_named_keys_changed(
    original: &Map<String, Value>,
    after: &str,
    chosen: &[&Setting],
) -> Result<(), Failure> {
    let internal = || {
        Failure::new(
            ErrorCode::IoFailure,
            "The planned app.json did not keep every other setting unchanged. Nothing was written; report this.",
        )
        .at("--vault")
    };
    let Ok(Value::Object(result)) = serde_json::from_str::<Value>(after) else {
        return Err(internal());
    };
    let named: BTreeSet<&str> = chosen.iter().map(|setting| setting.key).collect();
    let expected_len = original.len()
        + named
            .iter()
            .filter(|key| !original.contains_key(**key))
            .count();
    let kept = original
        .iter()
        .filter(|(key, _)| !named.contains(key.as_str()))
        .all(|(key, value)| result.get(key) == Some(value));
    let set = chosen
        .iter()
        .all(|setting| result.get(setting.key) == Some(&proposed(setting)));
    if kept && set && result.len() == expected_len {
        Ok(())
    } else {
        Err(internal())
    }
}

fn utf8(bytes: Vec<u8>) -> Result<String, Failure> {
    String::from_utf8(bytes).map_err(|_| {
        refusal(
            "NOT_UTF8",
            "--vault",
            "app.json is not UTF-8 text, so it is left alone.",
        )
    })
}

pub(crate) fn run(command: ObsidianCommand) -> Result<Output, Failure> {
    match command {
        ObsidianCommand::Setup(args) => setup(&args),
    }
}

fn setup(args: &SetupArgs) -> Result<Output, Failure> {
    let chosen = selected(&args.settings)?;
    if !is_real_dir(&args.vault) {
        return Err(refusal(
            "VAULT_NOT_A_FOLDER",
            "--vault",
            "The vault must be an existing folder and not a symbolic link.",
        ));
    }
    let (config_dir, source) = resolve_config_dir(&args.vault, args.config_dir.as_deref())?;
    let file =
        OwnedFile::open(&args.vault.join(&config_dir).join(SETTINGS_FILE)).map_err(file_failure)?;
    let before = file.read().map_err(file_failure)?.map(utf8).transpose()?;
    let after = splice(before.as_deref(), &chosen)?;
    let current: Option<Map<String, Value>> = before
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok());
    let rows: Vec<Value> = chosen
        .iter()
        .map(|setting| {
            let value = proposed(setting);
            let existing = current.as_ref().and_then(|map| map.get(setting.key));
            let action = match existing {
                None => "add",
                Some(now) if *now == value => "unchanged",
                Some(_) => "change",
            };
            json!({
                "key": setting.key,
                "proposed": value,
                "current": existing,
                "action": action,
                "reason": setting.reason,
            })
        })
        .collect();
    let changed = before.as_deref() != Some(after.as_str());
    let relative = format!("{config_dir}/{SETTINGS_FILE}");
    let diff = unified_diff(&relative, before.as_deref(), Some(&after));
    if changed && args.apply {
        file.replace(before.as_deref().map(str::as_bytes), after.as_bytes())
            .map_err(file_failure)?;
    }
    let written = changed && args.apply;
    let vault = args.vault.display().to_string();
    let mut apply_argv = vec![
        PROGRAM_NAME.to_owned(),
        "obsidian".into(),
        "setup".into(),
        "--vault".into(),
        vault.clone(),
    ];
    if args.config_dir.is_some() {
        apply_argv.extend(["--config-dir".into(), config_dir.clone()]);
    }
    for setting in &args.settings {
        apply_argv.extend(["--setting".into(), setting.clone()]);
    }
    let mut preview_argv = apply_argv.clone();
    apply_argv.push("--apply".into());
    let status_argv: Vec<String> = [PROGRAM_NAME, "vault", "status", "--format", "json"]
        .map(str::to_owned)
        .to_vec();
    let next = if changed && !args.apply {
        json!([{"kind":"local_write","argv":apply_argv}])
    } else {
        preview_argv.push("--format".into());
        preview_argv.push("json".into());
        json!([{"kind":"local_read","argv":if written { status_argv.clone() } else { preview_argv.clone() }}])
    };
    let value = json!({
        "vault": vault,
        "config_dir": config_dir,
        "config_dir_source": source.as_str(),
        "file": relative,
        "file_exists": before.is_some(),
        "apply": args.apply,
        "changed": changed,
        "written": written,
        "settings": rows,
        "diff": diff,
        "before": before.as_ref().map(|text| json!({"bytes": text.len(), "digest": hash_bytes(text.as_bytes())})),
        "after": {"bytes": after.len(), "digest": hash_bytes(after.as_bytes())},
        "never_touched": NEVER_TOUCHED,
        "limit_bytes": MAX_OWNED_FILE_BYTES,
        "next": next,
    });
    let mut text = format!(
        "obsidian setup  {vault}  {relative} ({})  {}",
        source.as_str(),
        match (changed, args.apply) {
            (false, _) => "already set; nothing to change",
            (true, false) => "preview: nothing was written",
            (true, true) => "written",
        }
    );
    for row in &rows {
        text.push_str(&format!(
            "\n  {}  {}  {}",
            row["action"].as_str().unwrap_or_default(),
            row["key"].as_str().unwrap_or_default(),
            row["proposed"]
        ));
    }
    if !diff.is_empty() {
        text.push('\n');
        text.push_str(diff.trim_end_matches('\n'));
    }
    let next_argv: Vec<String> =
        serde_json::from_value(value["next"][0]["argv"].clone()).unwrap_or_default();
    text.push_str(&format!("\nNext: {}", display_argv(&next_argv)));
    Ok(Output::success(
        if args.apply {
            "obsidian_setup_applied"
        } else {
            "obsidian_setup_preview"
        },
        value,
        text,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The README table of proposed settings is exactly `SETTINGS`.
    #[test]
    fn readme_settings_table_matches_the_code() {
        let readme = include_str!("../../../obsidian/README.md");
        let section = readme
            .split("## Setup preview")
            .nth(1)
            .expect("the README has a Setup preview section");
        let rows: Vec<(String, String, String)> = section
            .lines()
            .filter(|line| line.starts_with("| `"))
            .map(|line| {
                let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
                (
                    cells[0].trim_matches('`').to_owned(),
                    cells[1].trim_matches('`').to_owned(),
                    cells[2].to_owned(),
                )
            })
            .collect();
        let expected: Vec<(String, String, String)> = SETTINGS
            .iter()
            .map(|setting| {
                (
                    setting.key.to_owned(),
                    setting.value.to_owned(),
                    setting.reason.to_owned(),
                )
            })
            .collect();
        assert_eq!(rows, expected);
        for never in NEVER_TOUCHED
            .iter()
            .filter(|item| !item.contains(' ') && (item.ends_with(".json") || item.ends_with('/')))
        {
            assert!(section.contains(&format!("`{never}`")), "{never}");
        }
    }

    #[test]
    fn no_setting_touches_plugins_or_restricted_mode() {
        for setting in SETTINGS {
            let key = setting.key.to_lowercase();
            assert!(
                !key.contains("plugin") && !key.contains("restricted"),
                "{}",
                setting.key
            );
            assert!(serde_json::from_str::<Value>(setting.value).is_ok());
        }
    }

    fn all() -> Vec<&'static Setting> {
        SETTINGS.iter().collect()
    }

    #[test]
    fn splice_keeps_every_other_byte() {
        let before = "{\n\t\"a\": [1, {\"k\": \"}\"}],\n\t\"newLinkFormat\": \"shortest\",\r\n\t\"z\":  true\n}\n";
        let after = splice(Some(before), &all()).unwrap();
        assert_eq!(
            after,
            "{\n\t\"a\": [1, {\"k\": \"}\"}],\n\t\"newLinkFormat\": \"absolute\",\r\n\t\"z\":  true,\n\t\"useMarkdownLinks\": false\n}\n"
        );
        assert_eq!(splice(Some(&after), &all()).unwrap(), after);
    }

    #[test]
    fn splice_handles_empty_objects_and_absent_files() {
        let fresh = "{\n  \"useMarkdownLinks\": false,\n  \"newLinkFormat\": \"absolute\"\n}\n";
        assert_eq!(splice(None, &all()).unwrap(), fresh);
        assert_eq!(splice(Some("  \n"), &all()).unwrap(), fresh);
        assert_eq!(
            splice(Some("{}"), &all()).unwrap(),
            "{\n  \"useMarkdownLinks\": false,\n  \"newLinkFormat\": \"absolute\"\n}"
        );
        assert_eq!(
            splice(Some("{\"x\":1}"), &all()).unwrap(),
            "{\"x\":1, \"useMarkdownLinks\": false, \"newLinkFormat\": \"absolute\"}"
        );
    }

    #[test]
    fn splice_refuses_what_it_cannot_place_safely() {
        for bad in [
            "[1]",
            "{\"a\":",
            "\"x\"",
            "{\"newLinkFormat\":1,\"newLinkFormat\":2}",
        ] {
            assert!(splice(Some(bad), &all()).is_err(), "{bad}");
        }
    }

    #[test]
    fn config_dir_names_are_single_hidden_folders() {
        for good in [".obsidian", ".notes-config"] {
            assert!(valid_config_dir(good), "{good}");
        }
        for bad in ["", ".", "..", "obsidian", ".a/b", "../.x", ".a\\b"] {
            assert!(!valid_config_dir(bad), "{bad}");
        }
    }
}
