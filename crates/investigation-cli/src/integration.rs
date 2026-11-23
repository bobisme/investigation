//! `integration show/apply/remove/inspect`: explicit, bounded installation of
//! the embedded agent instructions into a file the user names.
//!
//! A target is a generic file shape, never a product:
//!
//! - `agents-block`: a managed block inside a Markdown instructions file such
//!   as `AGENTS.md`. Begin and end markers carry the instructions version.
//!   Apply appends the block, or replaces the text between existing markers;
//!   every byte outside the block stays as it was. Remove takes out exactly
//!   what apply added, so the file returns to its original bytes.
//! - `skill-file`: a file that holds only the optional skill. Apply creates
//!   it, or replaces it only when it already holds a version of the skill;
//!   remove deletes it on the same condition.
//!
//! Nothing here runs implicitly: `init`, `onboard`, `docs` and every read
//! leave instruction files alone, and `show` and `inspect` never write. Apply
//! and remove print a unified diff, accept `--dry-run`, never follow a
//! symbolic link ([`OwnedFile`]), and refuse a path inside an investigation
//! workspace, where an attachment named `AGENTS.md` is evidence, not
//! instructions.

use std::path::{Path, PathBuf};

use investigation_core::{
    PROGRAM_NAME,
    wire::{ErrorCode, Failure},
};
use investigation_store::{
    Workspace,
    journal::hash_bytes,
    owned_file::{MAX_OWNED_FILE_BYTES, OwnedFile, OwnedFileError},
};
use serde_json::{Value, json};

use crate::{Output, display_argv, docs, doctor};

pub(crate) const AFTER_HELP: &str = "\
Installs the embedded agent instructions only when asked, and only into the
file you name. show and inspect write nothing; apply and remove print a diff,
change only the managed block or the owned skill file, and accept --dry-run.
Examples:
  investigation integration show --target agents-block
  investigation integration apply --target agents-block --path AGENTS.md --dry-run
  investigation integration remove --target agents-block --path AGENTS.md
  investigation integration inspect --format json";

const APPLY_HELP: &str = "\
agents-block appends a managed block to FILE, or replaces the text between its
markers; every byte outside the block is kept. skill-file writes FILE only when
it is absent or already holds a version of the skill. Applying twice changes
nothing. FILE's directory must exist; links and workspace paths are refused.
Example: investigation integration apply --target agents-block --path AGENTS.md";

const REMOVE_HELP: &str = "\
agents-block deletes the managed block and the line break apply added before
it, which restores FILE's original bytes. skill-file deletes FILE when it holds
a version of the skill. Nothing installed is not an error.
Example: investigation integration remove --target agents-block --path AGENTS.md";

#[derive(clap::Subcommand)]
pub(crate) enum IntegrationCommand {
    /// Print exactly what apply would write for a target; writes nothing.
    #[command(
        after_help = "Example: investigation integration show --target agents-block --format json"
    )]
    Show {
        #[arg(long, value_enum)]
        target: Target,
    },
    /// Write the instructions into a managed block or an owned file you name.
    #[command(after_help = APPLY_HELP)]
    Apply(ChangeArgs),
    /// Take out what apply wrote, restoring the file's original bytes.
    #[command(after_help = REMOVE_HELP)]
    Remove(ChangeArgs),
    /// List instruction targets, versions, limits, selector validation and packs; writes nothing.
    #[command(after_help = "Example: investigation integration inspect --format json")]
    Inspect,
}

#[derive(clap::Args)]
pub(crate) struct ChangeArgs {
    #[arg(long, value_enum)]
    target: Target,
    /// The instructions file to change; nothing else is written.
    #[arg(long, value_name = "FILE")]
    path: PathBuf,
    /// Print the diff without writing.
    #[arg(long)]
    dry_run: bool,
}

/// A generic file shape that can hold the agent instructions.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(crate) enum Target {
    /// A managed block inside a Markdown instructions file, such as AGENTS.md.
    AgentsBlock,
    /// A file that holds only the optional skill.
    SkillFile,
}

const TARGETS: [Target; 2] = [Target::AgentsBlock, Target::SkillFile];

impl Target {
    const fn name(self) -> &'static str {
        match self {
            Self::AgentsBlock => "agents-block",
            Self::SkillFile => "skill-file",
        }
    }

    const fn shape(self) -> &'static str {
        match self {
            Self::AgentsBlock => "managed_block",
            Self::SkillFile => "owned_file",
        }
    }

    const fn summary(self) -> &'static str {
        match self {
            Self::AgentsBlock => {
                "A managed block in a Markdown instructions file such as AGENTS.md; text outside the markers is never changed."
            }
            Self::SkillFile => {
                "A file holding only the optional skill, for a skill directory; replaced or removed only while it holds a version of the skill."
            }
        }
    }

    fn page(self) -> &'static docs::Page {
        let name = match self {
            Self::AgentsBlock => "agents",
            Self::SkillFile => "skill",
        };
        docs::page(name).expect("the target's page is embedded")
    }

    /// The exact text apply writes: the block with its markers, or the file.
    fn content(self) -> String {
        let page = self.page();
        match self {
            Self::AgentsBlock => block(page.version, page.body),
            Self::SkillFile => page.body.to_owned(),
        }
    }
}

fn begin_prefix() -> String {
    format!("<!-- {PROGRAM_NAME}:begin ")
}

fn end_prefix() -> String {
    format!("<!-- {PROGRAM_NAME}:end ")
}

fn block(version: &str, body: &str) -> String {
    let mut text = format!(
        "{}{version} -->\n<!-- Managed block: `{PROGRAM_NAME} integration apply` replaces the text between these markers and `{PROGRAM_NAME} integration remove` deletes it. -->\n{body}",
        begin_prefix()
    );
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("{}{version} -->\n", end_prefix()));
    text
}

/// Byte range of the managed block: from the start of the begin marker line
/// to the end of the end marker line, its line break included.
#[derive(Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
    version: String,
}

/// The one managed block in `text`, if any. Unpaired, repeated or reversed
/// markers are an error: apply and remove then change nothing.
fn find_block(text: &str) -> Result<Option<Span>, String> {
    let (begin, end) = (begin_prefix(), end_prefix());
    let mut begins = Vec::new();
    let mut ends = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        for (prefix, found) in [(&begin, &mut begins), (&end, &mut ends)] {
            if let Some(rest) = bare.strip_prefix(prefix.as_str()) {
                let version = rest
                    .strip_suffix(" -->")
                    .filter(|v| !v.is_empty() && !v.contains(char::is_whitespace))
                    .ok_or_else(|| format!("the marker line `{bare}` is malformed"))?;
                found.push((offset, offset + line.len(), version.to_owned()));
            }
        }
        offset += line.len();
    }
    match (begins.as_slice(), ends.as_slice()) {
        ([], []) => Ok(None),
        ([(start, _, version)], [(_, end, end_version)]) if start < end => {
            if version != end_version {
                return Err(format!(
                    "the begin marker names {version} but the end marker names {end_version}"
                ));
            }
            Ok(Some(Span {
                start: *start,
                end: *end,
                version: version.clone(),
            }))
        }
        _ => Err(format!(
            "it holds {} begin and {} end marker line(s); exactly one of each, begin first, is needed",
            begins.len(),
            ends.len()
        )),
    }
}

/// What a change does to the file: its new bytes (`None`: no file) and a word
/// for the action.
struct Plan {
    after: Option<String>,
    action: &'static str,
    installed_version: Option<String>,
}

fn plan_apply(target: Target, before: Option<&str>) -> Result<Plan, Failure> {
    let content = target.content();
    let Some(text) = before else {
        return Ok(Plan {
            after: Some(content),
            action: "created",
            installed_version: None,
        });
    };
    match target {
        Target::AgentsBlock => {
            let span = find_block(text).map_err(malformed)?;
            Ok(match span {
                Some(span) => {
                    let mut after = text[..span.start].to_owned();
                    after.push_str(&content);
                    after.push_str(&text[span.end..]);
                    Plan {
                        action: if after == text {
                            "unchanged"
                        } else {
                            "replaced"
                        },
                        after: Some(after),
                        installed_version: Some(span.version),
                    }
                }
                // One line break separates the block from the existing text;
                // remove takes it out again with the block.
                None => Plan {
                    after: Some(format!("{text}\n{content}")),
                    action: "appended",
                    installed_version: None,
                },
            })
        }
        Target::SkillFile => {
            let installed = skill_version(text).ok_or_else(not_owned)?;
            Ok(Plan {
                action: if text == content {
                    "unchanged"
                } else {
                    "replaced"
                },
                after: Some(content),
                installed_version: Some(installed),
            })
        }
    }
}

fn plan_remove(target: Target, before: Option<&str>) -> Result<Plan, Failure> {
    let Some(text) = before else {
        return Ok(Plan {
            after: None,
            action: "not_installed",
            installed_version: None,
        });
    };
    match target {
        Target::AgentsBlock => {
            let Some(span) = find_block(text).map_err(malformed)? else {
                return Ok(Plan {
                    after: Some(text.to_owned()),
                    action: "not_installed",
                    installed_version: None,
                });
            };
            // Apply put one line break before a block it appended.
            let start = if span.start > 0 && text[..span.start].ends_with('\n') {
                span.start - 1
            } else {
                span.start
            };
            let rest = format!("{}{}", &text[..start], &text[span.end..]);
            // A file apply created held only the block: delete it again.
            let after = (span.start != 0 || !rest.is_empty()).then_some(rest);
            Ok(Plan {
                after,
                action: "removed",
                installed_version: Some(span.version),
            })
        }
        Target::SkillFile => {
            let installed = skill_version(text).ok_or_else(not_owned)?;
            Ok(Plan {
                after: None,
                action: "removed",
                installed_version: Some(installed),
            })
        }
    }
}

/// The instructions version of a skill file this command wrote: front matter
/// naming the program, then the version marker line. `None` for any other file.
fn skill_version(text: &str) -> Option<String> {
    let front = text.strip_prefix("---\n")?;
    let (matter, rest) = front.split_once("\n---\n")?;
    if !matter
        .lines()
        .any(|line| line == format!("name: {PROGRAM_NAME}"))
    {
        return None;
    }
    let marker = rest.lines().next()?;
    let version = marker.strip_prefix("<!-- ")?.strip_suffix(" -->")?;
    let family = docs::AGENT_INSTRUCTIONS_VERSION
        .rsplit_once('.')
        .map_or("", |(family, _)| family);
    version
        .strip_prefix(family)
        .filter(|suffix| suffix.starts_with(".v"))
        .map(|_| version.to_owned())
}

fn refusal(reason: &str, message: impl AsRef<str>) -> Failure {
    let mut failure = Failure::new(
        ErrorCode::InvalidInput,
        format!("{reason}: {} Nothing was written.", message.as_ref()),
    )
    .at("--path");
    failure.error.expected = Some(json!({ "reason": reason }));
    failure
}

fn malformed(detail: String) -> Failure {
    refusal(
        "BLOCK_MALFORMED",
        format!(
            "The file's managed block cannot be located safely: {detail}. Fix the markers by hand."
        ),
    )
}

fn not_owned() -> Failure {
    refusal(
        "NOT_OWNED",
        "The file exists and does not hold a version of the skill, so it is left alone. Name a new file.",
    )
}

fn file_failure(error: OwnedFileError) -> Failure {
    let (reason, message) = match &error {
        OwnedFileError::InvalidPath => (
            "INVALID_PATH",
            "The path must end in a file name.".to_owned(),
        ),
        OwnedFileError::Symlink => (
            "SYMLINK_REFUSED",
            "The path or one of the directories it names is a symbolic link, and integration never writes through a link. Name the real location.".to_owned(),
        ),
        OwnedFileError::DirectoryMissing => (
            "DIRECTORY_MISSING",
            "The file's directory does not exist. Create it first; integration creates no directories.".to_owned(),
        ),
        OwnedFileError::NotRegular => (
            "NOT_REGULAR_FILE",
            "The path names something other than a regular file.".to_owned(),
        ),
        OwnedFileError::MultipleLinks => (
            "MULTIPLE_LINKS",
            "The file has more than one hard link; replacing it would leave the other names stale.".to_owned(),
        ),
        OwnedFileError::TooLarge { size, limit } => (
            "FILE_TOO_LARGE",
            format!("The file holds {size} bytes; integration.file_bytes allows {limit}."),
        ),
        OwnedFileError::Unsupported => (
            "OPENAT2_UNSUPPORTED",
            "This kernel lacks openat2, so the path cannot be opened without following links.".to_owned(),
        ),
        OwnedFileError::Changed => {
            let mut failure = Failure::new(
                ErrorCode::IoFailure,
                "FILE_CHANGED: The file changed while it was being updated. Nothing was written; run the command again.",
            )
            .at("--path");
            failure.error.expected = Some(json!({ "reason": "FILE_CHANGED" }));
            return failure;
        }
        OwnedFileError::Io(io) => {
            return Failure::new(ErrorCode::IoFailure, format!("Change the file: {io}."))
                .at("--path");
        }
    };
    refusal(reason, message)
}

/// Open the named file, refusing one inside an investigation workspace.
fn open_target(path: &Path) -> Result<OwnedFile, Failure> {
    let file = OwnedFile::open(path).map_err(file_failure)?;
    let directory = std::fs::canonicalize(file.directory_path()).map_err(|error| {
        Failure::new(
            ErrorCode::IoFailure,
            format!("Resolve the file's directory: {error}."),
        )
        .at("--path")
    })?;
    if Workspace::locate(&directory).is_ok() {
        return Err(refusal(
            "INSIDE_WORKSPACE",
            "The path lies inside an investigation workspace. Files there are evidence or generated state, never agent instructions. Name a file outside the workspace.",
        ));
    }
    Ok(file)
}

fn read_text(file: &OwnedFile) -> Result<Option<String>, Failure> {
    file.read()
        .map_err(file_failure)?
        .map(|bytes| {
            String::from_utf8(bytes).map_err(|_| {
                refusal(
                    "NOT_UTF8",
                    "The file is not UTF-8 text, so a managed block cannot be placed in it.",
                )
            })
        })
        .transpose()
}

/// A one-hunk unified diff: every change here is one contiguous region.
fn unified_diff(label: &str, before: Option<&str>, after: Option<&str>) -> String {
    if before == after {
        return String::new();
    }
    fn lines(text: Option<&str>) -> Vec<&str> {
        text.map(|t| t.split_inclusive('\n').collect())
            .unwrap_or_default()
    }
    let (old, new) = (lines(before), lines(after));
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    const CONTEXT: usize = 3;
    let start = prefix - prefix.min(CONTEXT);
    let trail = suffix.min(CONTEXT);
    let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
    let range = |len: usize| {
        if len == 0 {
            format!("{start},0")
        } else {
            format!("{},{len}", start + 1)
        }
    };
    let mut diff = format!(
        "--- {}\n+++ {}\n@@ -{} +{} @@\n",
        if before.is_some() { label } else { "/dev/null" },
        if after.is_some() { label } else { "/dev/null" },
        range(old_end + trail - start),
        range(new_end + trail - start),
    );
    let mut push = |mark: char, line: &str| {
        diff.push(mark);
        diff.push_str(line);
        if !line.ends_with('\n') {
            diff.push_str("\n\\ No newline at end of file\n");
        }
    };
    for line in &old[start..prefix] {
        push(' ', line);
    }
    for line in &old[prefix..old_end] {
        push('-', line);
    }
    for line in &new[prefix..new_end] {
        push('+', line);
    }
    for line in &old[old_end..old_end + trail] {
        push(' ', line);
    }
    diff
}

fn argv(parts: &[&str]) -> Vec<String> {
    std::iter::once(PROGRAM_NAME)
        .chain(parts.iter().copied())
        .map(str::to_owned)
        .collect()
}

fn describe(target: Target) -> Value {
    let page = target.page();
    let content = target.content();
    json!({
        "target": target.name(),
        "shape": target.shape(),
        "summary": target.summary(),
        "version": page.version,
        "digest": hash_bytes(content.as_bytes()),
        "bytes": content.len(),
        "apply_argv": argv(&["integration", "apply", "--target", target.name(), "--path", "FILE"]),
    })
}

pub(crate) fn run(command: IntegrationCommand) -> Result<Output, Failure> {
    match command {
        IntegrationCommand::Show { target } => {
            let content = target.content();
            let mut value = describe(target);
            value["content"] = json!(content);
            value["writes"] = json!("none: show prints the text; only apply writes");
            Ok(Output::success("integration_target", value, content))
        }
        IntegrationCommand::Apply(args) => change(args, false),
        IntegrationCommand::Remove(args) => change(args, true),
        IntegrationCommand::Inspect => Ok(inspect()),
    }
}

fn change(args: ChangeArgs, remove: bool) -> Result<Output, Failure> {
    let ChangeArgs {
        target,
        path,
        dry_run,
    } = args;
    let file = open_target(&path)?;
    let before = read_text(&file)?;
    let plan = if remove {
        plan_remove(target, before.as_deref())?
    } else {
        plan_apply(target, before.as_deref())?
    };
    let changed = plan.after != before;
    if changed && !dry_run {
        match &plan.after {
            Some(after) => file.replace(before.as_deref().map(str::as_bytes), after.as_bytes()),
            None => file.remove(before.as_deref().unwrap_or_default().as_bytes()),
        }
        .map_err(file_failure)?;
    }
    let label = path.display().to_string();
    let diff = unified_diff(&label, before.as_deref(), plan.after.as_deref());
    let state = |text: &Option<String>| match text {
        Some(text) => {
            json!({"exists": true, "bytes": text.len(), "digest": hash_bytes(text.as_bytes())})
        }
        None => json!({"exists": false}),
    };
    let other = if remove { "apply" } else { "remove" };
    let undo = argv(&[
        "integration",
        other,
        "--target",
        target.name(),
        "--path",
        &label,
    ]);
    let version = target.page().version;
    let value = json!({
        "target": target.name(),
        "shape": target.shape(),
        "version": version,
        "installed_version": plan.installed_version,
        "path": label,
        "action": plan.action,
        "changed": changed,
        "written": changed && !dry_run,
        "dry_run": dry_run,
        "before": state(&before),
        "after": state(&plan.after),
        "diff": diff,
        "reverse_argv": undo,
        "limit_bytes": MAX_OWNED_FILE_BYTES,
    });
    let command = if remove { "remove" } else { "apply" };
    let mut text = format!(
        "{command}  {}  {label}  {}{}",
        target.name(),
        plan.action,
        if remove {
            String::new()
        } else {
            format!(" ({version})")
        }
    );
    if dry_run && changed {
        text.push_str("\ndry run: nothing was written");
    }
    if !diff.is_empty() {
        text.push('\n');
        text.push_str(diff.trim_end_matches('\n'));
    }
    if changed {
        text.push_str(&format!(
            "\n{} with: {}",
            if remove {
                "Install again"
            } else {
                "Take it out"
            },
            display_argv(&undo)
        ));
    }
    let result = if remove {
        "integration_removed"
    } else {
        "integration_applied"
    };
    Ok(Output::success(result, value, text))
}

fn inspect() -> Output {
    let targets: Vec<Value> = TARGETS.into_iter().map(describe).collect();
    let pages: Vec<Value> = docs::PAGES
        .iter()
        .map(|page| json!({"page": page.name, "version": page.version, "digest": hash_bytes(page.body.as_bytes())}))
        .collect();
    let versions = doctor::versions();
    let capabilities = doctor::capabilities();
    let limits = doctor::limits();
    let mut text = format!("agent instructions  {}\n", docs::AGENT_INSTRUCTIONS_VERSION);
    for target in TARGETS {
        text.push_str(&format!(
            "target  {:<12}  {}\n",
            target.name(),
            target.summary()
        ));
    }
    let names = |value: &Value| -> String {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().or_else(|| item["pack_id"].as_str()))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    text.push_str(&format!(
        "binary  {PROGRAM_NAME} {}\nreaders  {}\nmethod packs (built in)  {}\nlimits  {} enforced bounds; --format json lists them\n",
        env!("CARGO_PKG_VERSION"),
        names(&capabilities["readers"]),
        names(&capabilities["method_packs"]["built_in"]),
        limits["enforced"].as_array().map_or(0, Vec::len),
    ));
    text.push_str(&format!(
        "Show a target: {}",
        display_argv(&argv(&["integration", "show", "--target", "agents-block"]))
    ));
    Output::success(
        "integration_inspect",
        json!({
            "agent_instructions": {
                "version": docs::AGENT_INSTRUCTIONS_VERSION,
                "pages": pages,
            },
            "targets": targets,
            "versions": versions,
            "capabilities": capabilities,
            "limits": limits,
            "writes": "none: inspect reads this build only; no workspace, file or external tool is examined",
        }),
        text,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINALS: &[&str] = &[
        "",
        "# Project\n",
        "# Project\n\nKeep tests green.\n",
        "no trailing newline",
        "ends with a blank line\n\n",
        "crlf\r\nlines\r\n",
    ];

    fn after(plan: &Plan) -> &str {
        plan.after.as_deref().unwrap()
    }

    #[test]
    fn apply_twice_is_a_no_op_and_remove_restores_every_original() {
        for target in TARGETS {
            let created = plan_apply(target, None).unwrap();
            assert_eq!(created.action, "created");
            let again = plan_apply(target, Some(after(&created))).unwrap();
            assert_eq!(again.action, "unchanged");
            assert_eq!(after(&again), after(&created));
            let removed = plan_remove(target, Some(after(&created))).unwrap();
            assert_eq!(removed.action, "removed");
            assert_eq!(removed.after, None, "{target:?}: a created file is deleted");
        }
        for original in ORIGINALS {
            let applied = plan_apply(Target::AgentsBlock, Some(original)).unwrap();
            assert_eq!(applied.action, "appended");
            let text = after(&applied);
            assert!(text.starts_with(original), "{original:?}: prefix kept");
            let again = plan_apply(Target::AgentsBlock, Some(text)).unwrap();
            assert_eq!(again.action, "unchanged");
            assert_eq!(after(&again), text);
            let removed = plan_remove(Target::AgentsBlock, Some(text)).unwrap();
            assert_eq!(removed.after.as_deref(), Some(*original), "{original:?}");
            let nothing = plan_remove(Target::AgentsBlock, Some(original)).unwrap();
            assert_eq!(nothing.action, "not_installed");
        }
    }

    #[test]
    fn a_moved_or_older_block_is_replaced_in_place() {
        let old = block("inv.agent-instructions.v0", "Old text.\n");
        let file = format!("# Top\n\n{old}\n## Below\nkept\n");
        let plan = plan_apply(Target::AgentsBlock, Some(&file)).unwrap();
        assert_eq!(plan.action, "replaced");
        assert_eq!(
            plan.installed_version.as_deref(),
            Some("inv.agent-instructions.v0")
        );
        let expected = format!(
            "# Top\n\n{}\n## Below\nkept\n",
            Target::AgentsBlock.content()
        );
        assert_eq!(after(&plan), expected);
        let removed = plan_remove(Target::AgentsBlock, Some(after(&plan))).unwrap();
        assert_eq!(after(&removed), "# Top\n\n## Below\nkept\n");
    }

    #[test]
    fn malformed_markers_change_nothing() {
        let block = Target::AgentsBlock.content();
        let begin_only = block.lines().next().unwrap().to_owned() + "\n";
        let end_only = block.lines().last().unwrap().to_owned() + "\n";
        for bad in [
            begin_only.clone(),
            end_only.clone(),
            format!("{block}{block}"),
            format!("{end_only}{begin_only}"),
            format!("{}-->\n{end_only}", begin_prefix()),
            block.replacen(
                &format!("{} -->", docs::AGENT_INSTRUCTIONS_VERSION),
                "inv.agent-instructions.v9 -->",
                1,
            ),
        ] {
            for failure in [
                plan_apply(Target::AgentsBlock, Some(&bad)).err(),
                plan_remove(Target::AgentsBlock, Some(&bad)).err(),
            ] {
                let failure = failure.unwrap_or_else(|| panic!("accepted {bad:?}"));
                assert_eq!(failure.error.expected.unwrap()["reason"], "BLOCK_MALFORMED");
            }
        }
    }

    #[test]
    fn a_skill_file_is_owned_only_when_it_holds_the_skill() {
        let skill = Target::SkillFile.content();
        assert_eq!(
            skill_version(&skill).as_deref(),
            Some(docs::AGENT_INSTRUCTIONS_VERSION)
        );
        let older = skill.replace(
            docs::AGENT_INSTRUCTIONS_VERSION,
            "inv.agent-instructions.v0",
        );
        assert_eq!(
            plan_apply(Target::SkillFile, Some(&older)).unwrap().action,
            "replaced"
        );
        let agents = plan_apply(Target::AgentsBlock, None).unwrap();
        for foreign in [
            "# My notes\n",
            "---\nname: other\n---\n<!-- inv.agent-instructions.v1 -->\n",
            after(&agents),
        ] {
            assert!(skill_version(foreign).is_none(), "{foreign:?}");
            for failure in [
                plan_apply(Target::SkillFile, Some(foreign)).err(),
                plan_remove(Target::SkillFile, Some(foreign)).err(),
            ] {
                assert_eq!(
                    failure.unwrap().error.expected.unwrap()["reason"],
                    "NOT_OWNED"
                );
            }
        }
    }

    #[test]
    fn the_diff_is_one_hunk_with_context() {
        let before = "a\nb\nc\nd\ne\n";
        let after = "a\nb\nc\nd\ne\n\nX\n";
        let diff = unified_diff("F", Some(before), Some(after));
        assert_eq!(diff, "--- F\n+++ F\n@@ -3,3 +3,5 @@\n c\n d\n e\n+\n+X\n");
        assert_eq!(
            unified_diff("F", None, Some("x")),
            "--- /dev/null\n+++ F\n@@ -0,0 +1,1 @@\n+x\n\\ No newline at end of file\n"
        );
        assert_eq!(
            unified_diff("F", Some("x\n"), None),
            "--- F\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-x\n"
        );
        assert_eq!(unified_diff("F", Some("same"), Some("same")), "");
    }

    #[test]
    fn the_block_carries_its_version_and_the_page_text() {
        let page = docs::page("agents").unwrap();
        let content = Target::AgentsBlock.content();
        let span = find_block(&content).unwrap().unwrap();
        assert_eq!((span.start, span.end), (0, content.len()));
        assert_eq!(span.version, page.version);
        assert!(content.contains(page.body));
    }
}
