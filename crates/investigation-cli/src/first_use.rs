//! The shortest valid path from an ordinary working directory to a receipt
//! that names a case and its retained source, and back for a successor.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
};

use investigation_core::{
    PROGRAM_NAME,
    wire::{ErrorCode, Failure},
};
use investigation_store::Workspace;
use serde_json::{Value, json};

/// A generic example actor. Callers can and should choose their own.
pub(crate) const EXAMPLE_ACTOR: &str = "agent:investigator";

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// `init` for a workspace beside the files in the working directory, with
/// that directory as the only read-only import root.
pub(crate) fn init_step() -> Value {
    json!({
        "kind":"initialize_workspace",
        "argv":argv(&[PROGRAM_NAME, "init", "./casework", "--no-vault", "--import-root", ".", "--format", "json"]),
        "effect":"Creates ./casework and lets it retain copies of files beneath the working directory. Nothing else is read."
    })
}

/// `start` with the question and file left for the caller to supply.
pub(crate) fn start_step(workspace: &str) -> Value {
    json!({
        "kind":"start_case_with_source",
        "argv":argv(&[PROGRAM_NAME, "--workspace", workspace, "--actor", EXAMPLE_ACTOR, "start", "QUESTION", "--attach", "FILE", "--format", "json"]),
        "unresolved_fields":["QUESTION", "FILE"],
        "effect":"Opens an exploratory case and retains FILE as its first source in one transaction. The receipt names both."
    })
}

/// Text for the two-command start path.
pub(crate) fn start_text(workspace: &str, init: bool) -> String {
    let mut text = String::from("Start a case from a local file:\n");
    if init {
        text.push_str(&format!(
            "  {PROGRAM_NAME} init ./casework --no-vault --import-root . --format json\n"
        ));
    }
    text.push_str(&format!(
        "  {PROGRAM_NAME} --workspace {workspace} --actor {EXAMPLE_ACTOR} start 'QUESTION' --attach FILE --format json\n\
         Replace QUESTION and FILE. Use your own actor name if you have one. The receipt names the case and the retained source."
    ));
    text
}

/// `init` on an existing workspace, which adds `directory` to its import roots.
pub(crate) fn add_import_root_argv(workspace: &Workspace, directory: &str) -> Vec<String> {
    let root = workspace.root().to_string_lossy();
    argv(&[
        PROGRAM_NAME,
        "init",
        &root,
        "--import-root",
        directory,
        "--format",
        "json",
    ])
}

/// Advice when no configured import root can supply a path.
pub(crate) fn import_root_advice(workspace: &Workspace) -> String {
    format!(
        "Add the file's directory with: {}",
        crate::display_argv(&add_import_root_argv(workspace, "DIR"))
    )
}

/// The observation template that follows a `start` receipt.
pub(crate) fn observe_step(workspace: &Workspace, actor: &str, case: &str, source: &str) -> Value {
    let mut argv = crate::workspace_argv(workspace);
    argv.extend(
        [
            "--actor",
            actor,
            "--case",
            case,
            "observe",
            "STATEMENT",
            "--source",
            source,
            "--quote",
            "EXACT TEXT",
            "--limitations",
            "[\"LIMITATION\"]",
            "--format",
            "json",
        ]
        .map(str::to_owned),
    );
    json!({
        "kind":"record_observation",
        "argv":argv,
        "unresolved_fields":["STATEMENT", "EXACT TEXT", "LIMITATION"],
        "effect":"Records what the source shows. EXACT TEXT must be copied from the source; LIMITATION says what the source cannot show."
    })
}

/// Configured import roots as absolute lexical paths.
fn configured_roots(workspace: &Workspace) -> Vec<PathBuf> {
    workspace
        .config()
        .import_roots
        .iter()
        .filter_map(|root| {
            lexical(&if root.is_absolute() {
                root.clone()
            } else {
                workspace.root().join(root)
            })
        })
        .collect()
}

/// Check a local FILE argument before ingest and return the path to submit.
/// A missing file is refused with the path it resolved to and the import
/// roots; a file outside every import root is refused with the `init`
/// command that adds its directory. `flag` names the argument, or `None` for
/// a positional one; the repair argv repeats the invocation with a found file.
pub(crate) fn check_attachment(
    workspace: &Workspace,
    given: &Path,
    flag: Option<&str>,
) -> Result<PathBuf, Failure> {
    let submitted = attachment_path(workspace, given);
    if given == Path::new("-") {
        return Ok(submitted);
    }
    let resolved = if given.is_absolute() {
        lexical(given)
    } else {
        env::current_dir()
            .ok()
            .and_then(|cwd| lexical(&cwd.join(given)))
    }
    .unwrap_or_else(|| given.to_owned());
    let roots = configured_roots(workspace);
    // Ingest also tries a relative path beneath each import root.
    let beneath_roots: Vec<PathBuf> = if given.is_absolute() {
        Vec::new()
    } else {
        roots
            .iter()
            .filter_map(|root| lexical(&root.join(given)))
            .filter(|path| path.is_file())
            .collect()
    };
    let exists = fs::symlink_metadata(&resolved).is_ok();
    let rewritten = submitted != given;
    let at = flag.unwrap_or("PATH");
    let detail = |failure: Failure| {
        let mut failure = failure.at(at);
        failure.error.actual = Some(json!({"resolved_path": resolved}));
        failure.error.expected = Some(json!({"import_roots": roots}));
        failure
    };
    let roots_text = if roots.is_empty() {
        "none are configured".to_owned()
    } else {
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !exists && (rewritten || beneath_roots.is_empty()) {
        let mut message = format!(
            "{} resolves to {}, which does not exist. Relative paths are resolved against the working directory. Import roots: {roots_text}.",
            given.display(),
            resolved.display()
        );
        let found = beneath_roots.first();
        if let Some(found) = found {
            message.push_str(&format!(" The file exists at {}.", found.display()));
        }
        let failure = Failure::new(ErrorCode::InvalidInput, message);
        return Err(detail(
            match found.and_then(|found| with_replaced_path(env::args_os(), flag, given, found)) {
                Some(argv) => failure.with_argv(argv),
                None => failure,
            },
        ));
    }
    let inside = roots.iter().any(|root| resolved.starts_with(root));
    if exists && !inside && beneath_roots.is_empty() {
        let directory = fs::canonicalize(&resolved)
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let directory = directory.as_deref().and_then(Path::to_str).unwrap_or("DIR");
        let repair = add_import_root_argv(workspace, directory);
        let failure = Failure::new(
            ErrorCode::InvalidInput,
            format!(
                "{} resolves to {}, which is outside the import roots ({roots_text}). Add its directory with: {}",
                given.display(),
                resolved.display(),
                crate::display_argv(&repair)
            ),
        )
        .with_argv(repair);
        return Err(detail(failure));
    }
    Ok(submitted)
}

/// The current invocation with `--actor` added, for a missing-actor repair.
/// Arguments that read stdin are not repeated, since stdin is consumed.
pub(crate) fn with_example_actor(args: impl Iterator<Item = OsString>) -> Option<Vec<String>> {
    let mut args = args.skip(1).map(|arg| arg.into_string().ok());
    let rest: Option<Vec<String>> = args.by_ref().collect();
    let rest = rest?;
    if rest
        .iter()
        .any(|arg| arg == "-" || arg == "--actor" || arg.starts_with("--actor="))
    {
        return None;
    }
    let mut argv = vec![
        PROGRAM_NAME.to_owned(),
        "--actor".to_owned(),
        EXAMPLE_ACTOR.to_owned(),
    ];
    argv.extend(rest);
    Some(argv)
}

/// Repair for an `--import-root` that names a file rather than a directory.
pub(crate) fn import_root_repair(root: &Path) -> Option<PathBuf> {
    fs::canonicalize(root)
        .ok()
        .filter(|path| path.is_file())
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

/// The current invocation with one `FLAG VALUE` pair's value replaced.
pub(crate) fn with_replaced_value(
    args: impl Iterator<Item = OsString>,
    flag: &str,
    old: &Path,
    new: &Path,
) -> Option<Vec<String>> {
    let mut argv: Vec<String> = args
        .skip(1)
        .map(OsString::into_string)
        .collect::<Result<_, _>>()
        .ok()?;
    let index = argv
        .windows(2)
        .position(|pair| pair[0] == flag && Path::new(&pair[1]) == old)?;
    argv[index + 1] = new.to_str()?.to_owned();
    argv.insert(0, PROGRAM_NAME.to_owned());
    Some(argv)
}

/// The current invocation with a path argument replaced: the value after
/// `flag`, or else the only argument equal to `old`.
fn with_replaced_path(
    args: impl Iterator<Item = OsString>,
    flag: Option<&str>,
    old: &Path,
    new: &Path,
) -> Option<Vec<String>> {
    if let Some(flag) = flag {
        return with_replaced_value(args, flag, old, new);
    }
    let mut argv: Vec<String> = args
        .skip(1)
        .map(OsString::into_string)
        .collect::<Result<_, _>>()
        .ok()?;
    let mut matches = argv
        .iter()
        .enumerate()
        .filter(|(_, arg)| Path::new(arg) == old)
        .map(|(index, _)| index);
    let index = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    argv[index] = new.to_str()?.to_owned();
    argv.insert(0, PROGRAM_NAME.to_owned());
    Some(argv)
}

/// Explain a shortcut quote that does not occur in the source. The text
/// argument doubles as the quote unless `--quote` is given.
pub(crate) fn explain_quote_failure(mut failure: Failure) -> Failure {
    if failure.error.message == "OBSERVATION_TEXT_QUOTE_NOT_FOUND" {
        failure.error.message.push_str(
            ": the quote must be copied exactly from the source text. TEXT is used as the quote unless --quote gives one.",
        );
    }
    failure
}

/// Immediate child directories of `directory` that hold a workspace config.
/// Symbolic links are not followed.
pub(crate) fn child_workspaces(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| {
            fs::symlink_metadata(path.join("investigation.toml")).is_ok_and(|meta| meta.is_file())
        })
        .collect();
    found.sort();
    found.truncate(8);
    found
}

/// Child workspaces of the working directory, as `./name` display paths.
pub(crate) fn nearby_workspaces() -> Vec<(PathBuf, String)> {
    let Ok(cwd) = env::current_dir() else {
        return Vec::new();
    };
    child_workspaces(&cwd)
        .into_iter()
        .map(|path| {
            let display = path
                .file_name()
                .map(|name| format!("./{}", name.to_string_lossy()))
                .unwrap_or_else(|| path.display().to_string());
            (path, display)
        })
        .collect()
}

fn lexical(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
        }
    }
    Some(normalized)
}

/// Resolve a relative attachment against the working directory when the
/// result lies beneath a configured import root. Otherwise keep the path as
/// given, so ingest keeps resolving it beneath each import root.
///
/// Beneath the first root the path stays relative, so the acquisition label
/// keeps the caller's short name. Beneath a later root it becomes absolute,
/// because ingest tries a relative path against every root in order.
pub(crate) fn attachment_path(workspace: &Workspace, path: &Path) -> PathBuf {
    if path.is_absolute() || path == Path::new("-") {
        return path.to_owned();
    }
    let Some(candidate) = env::current_dir()
        .ok()
        .and_then(|cwd| lexical(&cwd.join(path)))
    else {
        return path.to_owned();
    };
    let roots = workspace.config().import_roots.iter().map(|root| {
        lexical(&if root.is_absolute() {
            root.clone()
        } else {
            workspace.root().join(root)
        })
    });
    for (index, root) in roots.enumerate() {
        let Some(relative) =
            root.and_then(|root| candidate.strip_prefix(&root).ok().map(Path::to_path_buf))
        else {
            continue;
        };
        if relative.as_os_str().is_empty() {
            break;
        }
        return if index == 0 { relative } else { candidate };
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_repair_repeats_the_invocation_unless_stdin_was_read() {
        let args = |parts: &[&str]| parts.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            with_example_actor(
                args(&["/tools/investigation", "start", "Q?", "--attach", "a.txt"]).into_iter()
            ),
            Some(
                [
                    "investigation",
                    "--actor",
                    EXAMPLE_ACTOR,
                    "start",
                    "Q?",
                    "--attach",
                    "a.txt"
                ]
                .map(str::to_owned)
                .to_vec()
            )
        );
        assert_eq!(
            with_example_actor(
                args(&["investigation", "start", "Q?", "--attach", "-"]).into_iter()
            ),
            None
        );
    }

    #[test]
    fn lexical_normalization_refuses_to_escape_the_root() {
        assert_eq!(
            lexical(Path::new("/work/casework/../source.txt")),
            Some(PathBuf::from("/work/source.txt"))
        );
        assert_eq!(
            lexical(Path::new("/work/./source.txt")),
            Some(PathBuf::from("/work/source.txt"))
        );
        assert_eq!(lexical(Path::new("/..")), None);
    }

    #[test]
    fn child_workspaces_lists_only_directories_with_a_config() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("casework")).unwrap();
        fs::write(root.join("casework/investigation.toml"), "").unwrap();
        fs::create_dir(root.join("notes")).unwrap();
        fs::write(root.join("investigation.toml"), "").unwrap();
        assert_eq!(child_workspaces(root), vec![root.join("casework")]);
    }
}
