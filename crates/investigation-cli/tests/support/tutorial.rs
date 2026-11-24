//! The tutorial runner: executes documented shell commands against the built
//! binary and checks the output a document quotes.
//!
//! One runner serves the shipped tutorial path (plan §17.5, `tests/demo.rs`)
//! and the method walkthroughs (`docs/walkthroughs/`, `tests/walkthroughs.rs`).
//! A document is Markdown. Each ```` ```sh ```` block is run in order, in one
//! temporary directory:
//!
//! - A line that starts with the program name (`investigation`, or `inv` in
//!   the plan) is one command. A trailing ` # comment` is not part of it, a
//!   trailing `\` continues it on the next line, and words are split the way a
//!   POSIX shell splits them for single and double quotes. `<<'EOF'` at the
//!   end feeds the following lines, up to `EOF`, to the command's stdin.
//! - `cat > PATH <<'EOF'` writes the following lines to PATH, and `cat PATH`
//!   prints a file, such as an export the previous command wrote.
//! - A comment line that says `exit status N` expects that status; every
//!   other command must succeed. A command with a non-zero status, and every
//!   `--dry-run`, must leave the canonical `state/` of every workspace in the
//!   directory byte-identical: a refusal commits nothing.
//! - A block whose first line is a comment that starts with `Schematic`
//!   (`# Schematic`, `// Schematic`) is shown, not run.
//!   Every other fenced block must be `sh` or a `text` block that quotes the
//!   output of the `sh` block before it, or start with a `Schematic` line, so
//!   nothing in a document looks executable without being executed.
//!
//! A ```` ```text ```` block right after a `sh` block quotes the output of its
//! last command (stdout, then stderr), after two substitutions that make it
//! reproducible: the absolute path of a workspace `demo` created reads
//! `<WORKSPACE>`, and an ID that a quoted line binds reads `$NAME`.
//!
//! - Every quoted line must match an output line exactly, apart from trailing
//!   spaces, in order. A line that is just `...` stands for any number of
//!   lines; without one at the start or end, the quote must start or end with
//!   the output. A line ending in ` ...` matches any line that starts with
//!   the text before it.
//! - `$NAME` (capital letters, digits and `_`) in a quoted line matches one
//!   word of the output and binds NAME to it, or must equal NAME's value once
//!   bound. Later commands substitute `$NAME` as a shell would. This is how a
//!   walkthrough uses the real IDs that a write returns.
//!
//! Bless mode ([`BLESS_ENV`]) rewrites quotes that no longer match instead of
//! failing, keeping each quote's shape ([`Session::bless`], [`blessed`]): a
//! line that still matches is kept as written, and a changed line is
//! replaced by the printed line it aligns with, with the `$NAME`s and the
//! ` ...` cut of the line it replaces. A changed ID-like word is also
//! replaced in the rest of the document, so a later command that names it
//! (`cat` of an export folder) follows. A quote whose lines cannot be paired
//! with printed lines one to one fails, naming the line, rather than guess.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const BIN: &str = env!("CARGO_BIN_EXE_investigation");

/// Set to rewrite quoted output that no longer matches, in place, for review
/// as a diff: `INVESTIGATION_BLESS_WALKTHROUGHS=1 cargo test -p
/// investigation-cli --test walkthroughs`.
pub const BLESS_ENV: &str = "INVESTIGATION_BLESS_WALKTHROUGHS";

/// Whether bless mode is switched on.
pub fn blessing() -> bool {
    std::env::var_os(BLESS_ENV).is_some()
}

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Run the binary in `cwd` with a clean environment; `(status, stdout, stderr)`.
pub fn run(cwd: &Path, args: &[String]) -> (i32, String, String) {
    run_with(cwd, args, None)
}

fn run_with(cwd: &Path, args: &[String], stdin: Option<&str>) -> (i32, String, String) {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .env_remove("INVESTIGATION_SESSION")
        .env_remove("INVESTIGATION_LOG")
        .env("TZ", "UTC")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// Every file under `root/relative`, by path, skipping machine-local lock and
/// temporary files and the disposable cache.
pub fn files(root: &Path, relative: &str) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let name = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let skipped = name
                .split('/')
                .any(|part| matches!(part, "locks" | "tmp" | "cache") || part.ends_with(".lock"));
            if skipped {
                continue;
            }
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &root.join(relative), &mut out);
    out
}

/// One executable action of a document.
#[derive(Clone, Debug)]
pub enum Action {
    /// The binary with these arguments (program name removed).
    Command {
        argv: Vec<String>,
        /// A here-document, and whether `$NAME` is substituted in it (an
        /// unquoted marker, as in a shell).
        stdin: Option<(String, bool)>,
    },
    /// `cat > PATH <<'EOF'`.
    Write { path: String, text: String },
    /// `cat PATH`: its output is the file.
    Read { path: String },
}

/// One `sh` block: its actions, the status its last command must exit with,
/// and the quoted output of that command.
#[derive(Clone, Debug)]
pub struct Step {
    /// `document:line` of the block, for failure messages.
    pub origin: String,
    pub actions: Vec<Action>,
    pub exit: i32,
    pub quote: Option<Vec<String>>,
    /// Zero-based line of the document where the quote's first line is.
    pub quote_at: Option<usize>,
}

impl Step {
    /// The arguments of the last command in the block.
    pub fn argv(&self) -> &[String] {
        self.actions
            .iter()
            .rev()
            .find_map(|action| match action {
                Action::Command { argv, .. } => Some(argv.as_slice()),
                Action::Write { .. } | Action::Read { .. } => None,
            })
            .unwrap_or(&[])
    }
}

/// The executable steps of `markdown`, whose commands start with `program`.
/// Panics, naming the line, on a fenced block that is neither run nor
/// labelled schematic, and on a command line the runner cannot read.
pub fn parse(markdown: &str, document: &str, program: &str) -> Vec<Step> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut steps: Vec<Step> = Vec::new();
    let mut index = 0;
    // Whether the last block was a `sh` block with nothing but blank lines since.
    let mut quotable = false;
    while index < lines.len() {
        let line = lines[index];
        let Some(info) = line.strip_prefix("```") else {
            if !line.trim().is_empty() {
                quotable = false;
            }
            index += 1;
            continue;
        };
        let start = index + 1;
        let end = (start..lines.len())
            .find(|&at| lines[at].starts_with("```"))
            .unwrap_or_else(|| panic!("{document}:{start}: unclosed code block"));
        let body = &lines[start..end];
        let origin = format!("{document}:{start}");
        let schematic = body.first().is_some_and(|first| {
            first
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .starts_with("Schematic")
        });
        match info.trim() {
            _ if schematic => quotable = false,
            "sh" => {
                steps.push(step(body, &origin, program));
                quotable = true;
            }
            "text" if quotable => {
                let step = steps.last_mut().unwrap();
                assert!(
                    step.quote.is_none(),
                    "{origin}: a second output block for one command"
                );
                step.quote = Some(body.iter().map(|line| (*line).to_owned()).collect());
                step.quote_at = Some(start);
                quotable = false;
            }
            other => panic!(
                "{origin}: a ```{other} block that is neither run nor labelled schematic; \
                 start it with a `Schematic` line, or make it a sh block or the text \
                 output right after one"
            ),
        }
        index = end + 1;
    }
    steps
}

fn step(body: &[&str], origin: &str, program: &str) -> Step {
    let mut actions = Vec::new();
    let mut exit = 0;
    let mut at = 0;
    while at < body.len() {
        let mut line = body[at].trim().to_owned();
        at += 1;
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if let Some(rest) = comment.split("exit status ").nth(1) {
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                exit = digits
                    .parse()
                    .unwrap_or_else(|_| panic!("{origin}: unreadable `exit status` comment"));
            }
            continue;
        }
        while let Some(continued) = line.strip_suffix('\\') {
            let next = body
                .get(at)
                .unwrap_or_else(|| panic!("{origin}: `\\` on the last line"));
            line = format!("{} {}", continued.trim_end(), next.trim());
            at += 1;
        }
        let command = strip_comment(&line);
        let mut words = split(command, origin);
        let heredoc = match words.last().map(String::as_str) {
            Some(last) if last.starts_with("<<") => {
                let marker = last.trim_start_matches('<').to_owned();
                // As in a shell, a quoted marker turns substitution off.
                let expands = !command.ends_with(['\'', '"']);
                words.pop();
                let from = at;
                let until = (from..body.len())
                    .find(|&index| body[index].trim() == marker)
                    .unwrap_or_else(|| panic!("{origin}: here-document without {marker}"));
                at = until + 1;
                let mut text = body[from..until].join("\n");
                text.push('\n');
                Some((text, expands))
            }
            _ => None,
        };
        match words.first().map(String::as_str) {
            Some("cat") if words.len() == 3 && words[1] == ">" => actions.push(Action::Write {
                path: words[2].clone(),
                text: heredoc
                    .unwrap_or_else(|| panic!("{origin}: `cat >` needs a here-document"))
                    .0,
            }),
            Some("cat") if words.len() == 2 && heredoc.is_none() => actions.push(Action::Read {
                path: words[1].clone(),
            }),
            Some(first) if first == program => actions.push(Action::Command {
                argv: words[1..].to_vec(),
                stdin: heredoc,
            }),
            _ => panic!("{origin}: `{line}` is not a `{program}` command"),
        }
    }
    assert!(!actions.is_empty(), "{origin}: an empty sh block");
    Step {
        origin: origin.to_owned(),
        actions,
        exit,
        quote: None,
        quote_at: None,
    }
}

/// The line without a trailing ` # comment` outside quotes.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut previous = ' ';
    for (at, character) in line.char_indices() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), _) if character == open => quote = None,
            (None, '#') if previous.is_whitespace() => return line[..at].trim_end(),
            _ => {}
        }
        previous = character;
    }
    line
}

/// Shell words with quotes removed. `$NAME` stays as written (single-quoted
/// text is marked so it is never substituted).
fn split(line: &str, origin: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        match character {
            '\'' => {
                started = true;
                for inner in chars.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                    // A literal `$` cannot start a substitution.
                    word.push(if inner == '$' { LITERAL_DOLLAR } else { inner });
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => word.extend(chars.next()),
                        Some(inner) => word.push(inner),
                        None => panic!("{origin}: unclosed double quote"),
                    }
                }
            }
            '\\' => {
                started = true;
                word.extend(chars.next());
            }
            space if space.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            other => {
                started = true;
                word.push(other);
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

const LITERAL_DOLLAR: char = '\u{E024}';

/// Runs steps in one directory, keeping the IDs quoted lines bound.
pub struct Session {
    cwd: PathBuf,
    workspaces: Vec<String>,
    variables: BTreeMap<String, String>,
    /// In bless mode, what to change in the document.
    bless: Option<Blessing>,
}

/// The changes bless mode found: quotes to replace and words to rename.
#[derive(Debug, Default)]
pub struct Blessing {
    pub rewrites: Vec<Rewrite>,
    /// `(line, old, new)`: from document line `line` on, `old` reads `new`.
    pub renames: Vec<(usize, String, String)>,
}

/// One quote to replace: its first line, how many lines it had, and the
/// lines that replace them.
#[derive(Debug)]
pub struct Rewrite {
    pub origin: String,
    pub at: usize,
    pub len: usize,
    pub lines: Vec<String>,
}

/// What a step printed, normalised.
pub struct Outcome {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Session {
    pub fn new(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            workspaces: Vec::new(),
            variables: BTreeMap::new(),
            bless: None,
        }
    }

    /// Rewrite quotes that no longer match instead of failing; the changes
    /// are collected for [`blessed`] ([`Self::blessing`]).
    pub fn bless(mut self) -> Self {
        self.bless = Some(Blessing::default());
        self
    }

    /// What bless mode found to change, if it is on.
    pub fn blessing(&self) -> Option<&Blessing> {
        self.bless.as_ref()
    }

    /// `text` with the words renamed so far.
    fn renamed(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (_, old, new) in self.bless.iter().flat_map(|bless| &bless.renames) {
            text = text.replace(old.as_str(), new);
        }
        text
    }

    /// The step as the blessed document will read: earlier renames applied.
    fn renamed_step(&self, step: &Step) -> Step {
        let mut step = step.clone();
        if self
            .bless
            .as_ref()
            .is_none_or(|bless| bless.renames.is_empty())
        {
            return step;
        }
        for action in &mut step.actions {
            match action {
                Action::Command { argv, stdin } => {
                    for word in argv.iter_mut() {
                        *word = self.renamed(word);
                    }
                    if let Some((text, _)) = stdin {
                        *text = self.renamed(text);
                    }
                }
                Action::Write { path, text } => {
                    *path = self.renamed(path);
                    *text = self.renamed(text);
                }
                Action::Read { path } => *path = self.renamed(path),
            }
        }
        if let Some(quote) = &mut step.quote {
            for line in quote.iter_mut() {
                *line = self.renamed(line);
            }
        }
        step
    }

    pub fn variables(&self) -> &BTreeMap<String, String> {
        &self.variables
    }

    /// Run one step and check its status, its quoted output and, when it is
    /// refused or a dry run, that it changed no canonical byte.
    pub fn run(&mut self, step: &Step) -> Outcome {
        let step = &self.renamed_step(step);
        let mut outcome = None;
        for action in &step.actions {
            match action {
                Action::Write { path, text } => {
                    let path = self.cwd.join(self.substitute(path, &step.origin));
                    fs::write(path, text).unwrap();
                }
                Action::Read { path } => {
                    let path = self.substitute(path, &step.origin);
                    let text = fs::read_to_string(self.cwd.join(&path))
                        .unwrap_or_else(|error| panic!("{}: cat {path}: {error}", step.origin));
                    outcome = Some((
                        vec!["cat".to_owned(), path],
                        Outcome {
                            status: 0,
                            stdout: self.normalise(&text),
                            stderr: String::new(),
                        },
                    ));
                }
                Action::Command { argv, stdin } => {
                    let argv: Vec<String> = argv
                        .iter()
                        .map(|word| self.substitute(word, &step.origin))
                        .collect();
                    let stdin = stdin.as_ref().map(|(text, expands)| {
                        if *expands {
                            self.substitute(text, &step.origin)
                        } else {
                            text.clone()
                        }
                    });
                    let before = self.canonical();
                    let (status, stdout, stderr) = run_with(&self.cwd, &argv, stdin.as_deref());
                    if argv.first().is_some_and(|first| first == "demo") && status == 0 {
                        self.workspaces.push(argv[1].clone());
                    }
                    let (stdout, stderr) = (self.normalise(&stdout), self.normalise(&stderr));
                    let refused = status != 0;
                    if refused || argv.iter().any(|word| word == "--dry-run") {
                        assert!(
                            before == self.canonical(),
                            "{}: `{}` exited {status} but changed canonical state\n{stdout}{stderr}",
                            step.origin,
                            argv.join(" ")
                        );
                    }
                    outcome = Some((
                        argv,
                        Outcome {
                            status,
                            stdout,
                            stderr,
                        },
                    ));
                }
            }
        }
        let Some((argv, outcome)) = outcome else {
            return Outcome {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            };
        };
        let command = argv.join(" ");
        assert_eq!(
            outcome.status, step.exit,
            "{}: `{command}` exited {}, the document says {}\n{}{}",
            step.origin, outcome.status, step.exit, outcome.stdout, outcome.stderr
        );
        if let Some(quote) = &step.quote {
            let printed = format!("{}{}", outcome.stdout, outcome.stderr);
            let result = self.matches(quote, &printed);
            if result.is_err() && self.bless.is_some() {
                self.bless_quote(step, quote, &printed);
            } else if let Err(message) = result {
                panic!(
                    "{}: the output of `{command}` no longer matches the walkthrough.\n\
                     {message}\n--- quoted ---\n{}\n--- printed (normalised) ---\n{printed}",
                    step.origin,
                    quote.join("\n")
                );
            }
        }
        outcome
    }

    /// Bless mode: replace `quote` by one that matches `printed` and keeps
    /// its shape, binding its names; panic, naming the line, when its lines
    /// cannot be paired with printed lines.
    fn bless_quote(&mut self, step: &Step, quote: &[String], printed: &str) {
        let at = step.quote_at.expect("a quote has a line");
        let lines = rewrite(quote, printed, &self.variables).unwrap_or_else(|reason| {
            panic!(
                "{}: bless cannot rewrite this quote mechanically: {reason}\n\
                 --- quoted ---\n{}\n--- printed (normalised) ---\n{printed}",
                step.origin,
                quote.join("\n")
            )
        });
        if let Err(message) = self.matches(&lines, printed) {
            panic!(
                "{}: the blessed quote does not match: {message}\n{}",
                step.origin,
                lines.join("\n")
            );
        }
        let end = at + quote.len();
        let bless = self.bless.as_mut().unwrap();
        for (old, new) in quote.iter().zip(&lines) {
            for (from, to) in renamed_words(old, new) {
                if !bless.renames.iter().any(|(_, known, _)| *known == from) {
                    bless.renames.push((end, from, to));
                }
            }
        }
        bless.rewrites.push(Rewrite {
            origin: step.origin.clone(),
            at,
            len: quote.len(),
            lines,
        });
    }

    /// Canonical bytes of every workspace in the directory.
    fn canonical(&self) -> BTreeMap<String, Vec<u8>> {
        let mut all = BTreeMap::new();
        for workspace in &self.workspaces {
            for (path, bytes) in files(&self.cwd.join(workspace), "state") {
                all.insert(format!("{workspace}/{path}"), bytes);
            }
        }
        all
    }

    fn substitute(&self, text: &str, origin: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find('$') {
            out.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            let braced = after.starts_with('{');
            let name: String = after
                .trim_start_matches('{')
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            if name.is_empty() {
                out.push('$');
                rest = after;
                continue;
            }
            let value = self.variables.get(&name).unwrap_or_else(|| {
                panic!("{origin}: ${name} is used before a quoted output line binds it")
            });
            out.push_str(value);
            rest = &after[name.len() + usize::from(braced) * 2..];
        }
        out.push_str(rest);
        out.replace(LITERAL_DOLLAR, "$")
    }

    fn normalise(&self, printed: &str) -> String {
        let mut text = printed.to_owned();
        let cwd = self.cwd.canonicalize().unwrap_or_else(|_| self.cwd.clone());
        for workspace in &self.workspaces {
            let relative = workspace.trim_start_matches("./");
            for base in [&cwd, &self.cwd] {
                let absolute = base.join(relative);
                text = text.replace(absolute.to_str().unwrap(), "<WORKSPACE>");
            }
        }
        text.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Match `quote` against `printed`, binding new `$NAME`s on success.
    fn matches(&mut self, quote: &[String], printed: &str) -> Result<(), String> {
        let actual: Vec<&str> = printed.lines().collect();
        let mut groups: Vec<&[String]> = Vec::new();
        let (mut open_start, mut open_end) = (false, false);
        let mut from = 0;
        for (index, line) in quote.iter().enumerate() {
            if line.trim() == "..." {
                if index == 0 {
                    open_start = true;
                }
                if index == quote.len() - 1 {
                    open_end = true;
                }
                if from < index {
                    groups.push(&quote[from..index]);
                }
                from = index + 1;
            }
        }
        if from < quote.len() {
            groups.push(&quote[from..]);
        }
        let mut bound = self.variables.clone();
        let mut at = 0;
        for (number, group) in groups.iter().enumerate() {
            let first = number == 0 && !open_start;
            let last = number == groups.len() - 1 && !open_end;
            let candidates: Vec<usize> = if first && last {
                (group.len() == actual.len())
                    .then_some(0)
                    .into_iter()
                    .collect()
            } else if first {
                vec![0]
            } else if last {
                actual.len().checked_sub(group.len()).into_iter().collect()
            } else {
                (at..=actual.len().saturating_sub(group.len())).collect()
            };
            let found = candidates
                .into_iter()
                .filter(|&start| start >= at)
                .find_map(|start| {
                    let mut trial = bound.clone();
                    let fits = start + group.len() <= actual.len()
                        && group
                            .iter()
                            .zip(&actual[start..])
                            .all(|(expected, line)| line_matches(expected, line, &mut trial));
                    fits.then_some((start, trial))
                });
            let Some((start, trial)) = found else {
                let place = if first {
                    "at the start of the output"
                } else if last {
                    "at the end of the output"
                } else {
                    "in the rest of the output"
                };
                return Err(format!(
                    "These quoted lines are not {place}:\n{}",
                    group.join("\n")
                ));
            };
            bound = trial;
            at = start + group.len();
        }
        if groups.is_empty() && !open_start && !actual.is_empty() {
            return Err("The quote is empty but the command printed output.".into());
        }
        self.variables = bound;
        Ok(())
    }
}

/// One quoted line against one printed line.
fn line_matches(expected: &str, actual: &str, bound: &mut BTreeMap<String, String>) -> bool {
    let expected = expected.trim_end();
    let (expected, prefix) = match expected.strip_suffix(" ...") {
        Some(head) => (head, true),
        None => (expected, false),
    };
    let mut rest = actual;
    let mut pattern = expected;
    loop {
        let Some(at) = pattern.find('$') else {
            return if prefix {
                rest.starts_with(pattern)
            } else {
                rest == pattern
            };
        };
        let name: String = pattern[at + 1..]
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        if name.is_empty() {
            let (head, tail) = pattern.split_at(at + 1);
            let Some(left) = rest.strip_prefix(head) else {
                return false;
            };
            rest = left;
            pattern = tail;
            continue;
        }
        let Some(left) = rest.strip_prefix(&pattern[..at]) else {
            return false;
        };
        let word: String = left
            .chars()
            .take_while(|c| !c.is_whitespace() && !matches!(c, '@' | ',' | ')' | '"'))
            .collect();
        if word.is_empty() {
            return false;
        }
        match bound.get(&name) {
            Some(value) if *value != word => return false,
            Some(_) => {}
            None => {
                bound.insert(name.clone(), word.clone());
            }
        }
        rest = &left[word.len()..];
        pattern = &pattern[at + 1 + name.len()..];
    }
}

/// `markdown` with `blessing` applied: each renamed word replaced in the
/// lines after the quote that changed it, then each quote replaced.
pub fn blessed(markdown: &str, blessing: &Blessing) -> String {
    let mut lines: Vec<String> = markdown.lines().map(str::to_owned).collect();
    for (from, old, new) in &blessing.renames {
        for line in lines.iter_mut().skip(*from) {
            *line = line.replace(old.as_str(), new);
        }
    }
    let mut rewrites: Vec<&Rewrite> = blessing.rewrites.iter().collect();
    rewrites.sort_by_key(|rewrite| std::cmp::Reverse(rewrite.at));
    for rewrite in rewrites {
        lines.splice(rewrite.at..rewrite.at + rewrite.len, rewrite.lines.clone());
    }
    let mut text = lines.join("\n");
    if markdown.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// A word that is safe to rename across a document: long and ID-like (it
/// has a digit and an `_`, `-` or `/`), and not a `$NAME`.
fn id_like(word: &str) -> bool {
    word.len() >= 12
        && !word.contains('$')
        && word.chars().any(|c| c.is_ascii_digit())
        && word.contains(['_', '-', '/'])
}

/// ID-like words that `old` has and `new` replaces at the same position:
/// both lines must have the same number of words.
fn renamed_words(old: &str, new: &str) -> Vec<(String, String)> {
    let old: Vec<&str> = old.trim_end_matches(" ...").split_whitespace().collect();
    let new: Vec<&str> = new.trim_end_matches(" ...").split_whitespace().collect();
    if old.len() != new.len() {
        return Vec::new();
    }
    old.iter()
        .zip(&new)
        .filter(|(a, b)| a != b && id_like(a) && id_like(b))
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

/// The words of a quoted line, `$NAME`s read as their bound value or as a
/// wildcard (`None`), without a ` ...` cut.
fn quoted_words(line: &str, bound: &BTreeMap<String, String>) -> Vec<Option<String>> {
    line.trim_end()
        .trim_end_matches(" ...")
        .split_whitespace()
        .map(|word| {
            let mut out = word.to_owned();
            for (name, value) in bound {
                out = out.replace(&format!("${name}"), value);
            }
            (!out.contains('$') || !out.chars().any(|c| c.is_ascii_uppercase())).then_some(out)
        })
        .collect()
}

/// How alike a quoted line and a printed line are: the share of words in
/// order they have in common (a wildcard matches any word).
fn likeness(quoted: &str, printed: &str, bound: &BTreeMap<String, String>) -> f64 {
    let left = quoted_words(quoted, bound);
    let cut = quoted.trim_end().ends_with(" ...");
    let mut right: Vec<&str> = printed.split_whitespace().collect();
    if cut {
        right.truncate(left.len());
    }
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    // Longest common subsequence of words.
    let mut table = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for (i, a) in left.iter().enumerate() {
        for (j, b) in right.iter().enumerate() {
            table[i + 1][j + 1] = if a.as_deref().is_none_or(|a| a == *b) {
                table[i][j] + 1
            } else {
                table[i][j + 1].max(table[i + 1][j])
            };
        }
    }
    table[left.len()][right.len()] as f64 / left.len().max(right.len()) as f64
}

/// The printed line `actual` in the shape of the quoted line `quoted`: cut
/// after as many words as the quote kept when it ends in ` ...`, and with
/// each `$NAME` of the quote in place of its value. New names are bound in
/// `bound`.
fn templated(
    quoted: &str,
    actual: &str,
    bound: &mut BTreeMap<String, String>,
) -> Result<String, String> {
    let quoted = quoted.trim_end();
    let (head, cut) = match quoted.strip_suffix(" ...") {
        Some(head) => (head, true),
        None => (quoted, false),
    };
    let mut line = actual.to_owned();
    if cut {
        let keep = head.split_whitespace().count();
        let mut words = 0;
        let mut previous_space = true;
        for (at, c) in actual.char_indices() {
            if !c.is_whitespace() && previous_space {
                words += 1;
                if words == keep + 1 {
                    line = format!("{} ...", actual[..at].trim_end());
                    break;
                }
            }
            previous_space = c.is_whitespace();
        }
    }
    // Put each `$NAME` back, left to right, after the literal text before it.
    let mut out = String::new();
    let mut rest = line.as_str();
    let mut pattern = head;
    while let Some(at) = pattern.find('$') {
        let name: String = pattern[at + 1..]
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        if name.is_empty() {
            pattern = &pattern[at + 1..];
            continue;
        }
        let before = &pattern[..at];
        let value = match bound.get(&name) {
            Some(value) => value.clone(),
            None => {
                let start = rest
                    .find(before)
                    .ok_or_else(|| format!("the text before ${name} is gone: {before:?}"))?
                    + before.len();
                let word: String = rest[start..]
                    .chars()
                    .take_while(|c| !c.is_whitespace() && !matches!(c, '@' | ',' | ')' | '"'))
                    .collect();
                if word.is_empty() {
                    return Err(format!("no word follows {before:?} for ${name}"));
                }
                bound.insert(name.clone(), word.clone());
                word
            }
        };
        let found = rest
            .find(&value)
            .ok_or_else(|| format!("${name} ({value}) is no longer printed on this line"))?;
        out.push_str(&rest[..found]);
        out.push('$');
        out.push_str(&name);
        rest = &rest[found + value.len()..];
        pattern = &pattern[at + 1 + name.len()..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A quote that matches `printed`, made from `quote`: its `...` lines stay,
/// each run of quoted lines is aligned with the printed lines it fits best
/// (every line pairs with a printed line at least half alike, in order and
/// without gaps), lines that still match stay as written and the others
/// become their printed line in the quoted line's shape. An error says why
/// no single alignment exists.
pub fn rewrite(
    quote: &[String],
    printed: &str,
    bound: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let actual: Vec<&str> = printed.lines().collect();
    let elided = |line: &String| line.trim() == "...";
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut from = 0;
    for (index, line) in quote.iter().enumerate() {
        if elided(line) {
            if from < index {
                groups.push((from, index));
            }
            from = index + 1;
        }
    }
    if from < quote.len() {
        groups.push((from, quote.len()));
    }
    let open_start = quote.first().is_some_and(elided);
    let open_end = quote.last().is_some_and(elided);
    let mut out: Vec<String> = quote.to_vec();
    let mut bound = bound.clone();
    // A quote of the whole output, with nothing cut or named, is the output.
    if groups.len() == 1
        && !open_start
        && !open_end
        && quote.len() != actual.len()
        && !quote
            .iter()
            .any(|line| line.contains('$') || line.trim_end().ends_with(" ..."))
    {
        return Ok(actual.iter().map(|line| (*line).to_owned()).collect());
    }
    let mut at = 0;
    for (number, &(start, end)) in groups.iter().enumerate() {
        let group = &quote[start..end];
        let first = number == 0 && !open_start;
        let last = number == groups.len() - 1 && !open_end;
        let candidates: Vec<usize> = if first && last {
            (group.len() == actual.len())
                .then_some(0)
                .into_iter()
                .collect()
        } else if first {
            vec![0]
        } else if last {
            actual.len().checked_sub(group.len()).into_iter().collect()
        } else {
            (at..=actual.len().saturating_sub(group.len())).collect()
        };
        let mut best: Vec<(usize, usize)> = Vec::new();
        let mut best_score = 0;
        for candidate in candidates.into_iter().filter(|&c| c >= at) {
            if candidate + group.len() > actual.len() {
                continue;
            }
            let mut trial = bound.clone();
            let mut score = 0;
            let mut fits = true;
            for (quoted, line) in group.iter().zip(&actual[candidate..]) {
                if line_matches(quoted, line, &mut trial) {
                    score += 2;
                } else if likeness(quoted, line, &trial) >= 0.5 {
                    score += 1;
                } else {
                    fits = false;
                    break;
                }
            }
            if !fits {
                continue;
            }
            match score.cmp(&best_score) {
                std::cmp::Ordering::Greater => {
                    best_score = score;
                    best = vec![(candidate, score)];
                }
                std::cmp::Ordering::Equal => best.push((candidate, score)),
                std::cmp::Ordering::Less => {}
            }
        }
        let place = || {
            format!(
                "quoted lines {}-{} ({:?}...)",
                start + 1,
                end,
                group[0].chars().take(60).collect::<String>()
            )
        };
        let (start_at, _) = match best.as_slice() {
            [] => {
                return Err(format!(
                    "{} pair with no run of printed lines line by line; lines were added, \\
                     removed or rewritten",
                    place()
                ));
            }
            [one] => *one,
            [one, ..] if best_score == 2 * group.len() => *one,
            _ => {
                return Err(format!(
                    "{} fit {} places in the output equally well",
                    place(),
                    best.len()
                ));
            }
        };
        for (offset, quoted) in group.iter().enumerate() {
            let line = actual[start_at + offset];
            let mut trial = bound.clone();
            if line_matches(quoted, line, &mut trial) {
                bound = trial;
            } else {
                out[start + offset] = templated(quoted, line, &mut bound)
                    .map_err(|reason| format!("quoted line {}: {reason}", start + offset + 1))?;
            }
        }
        at = start_at + group.len();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_lines_elide_bind_and_refuse_drift() {
        let mut session = Session::new(Path::new("."));
        let printed = "head\nart_1  revision 1\nmiddle\nart_2  revision 4\ntail";
        let quote = |lines: &[&str]| lines.iter().map(|l| (*l).to_owned()).collect::<Vec<_>>();
        assert!(
            session
                .matches(&quote(&["...", "$ONE  revision 1", "...", "tail"]), printed)
                .is_ok()
        );
        assert_eq!(session.variables()["ONE"], "art_1");
        // A bound name must keep its value.
        assert!(
            session
                .matches(&quote(&["...", "$ONE  revision 4", "..."]), printed)
                .is_err()
        );
        assert!(
            session
                .matches(&quote(&["head", "art_1 ..."]), printed)
                .is_err()
        );
        assert!(
            session
                .matches(&quote(&["head", "art_1 ...", "..."]), printed)
                .is_ok()
        );
        assert!(
            session
                .matches(&quote(&["...", "tail", "extra"]), printed)
                .is_err()
        );
    }

    #[test]
    fn only_schematic_blocks_are_shown_without_running() {
        let schematic = "```json\n// Schematic: not run.\n{}\n```\n\n```sh\n# Schematic\ninvestigation x\n```\n";
        assert!(parse(schematic, "doc", "investigation").is_empty());
        let unlabelled =
            std::panic::catch_unwind(|| parse("```json\n{}\n```\n", "doc", "investigation"));
        assert!(unlabelled.is_err());
        // An output block must follow the command it quotes.
        let orphan =
            std::panic::catch_unwind(|| parse("```text\nout\n```\n", "doc", "investigation"));
        assert!(orphan.is_err());
    }

    #[test]
    fn commands_split_like_a_shell_and_keep_here_documents() {
        let steps = parse(
            "```sh\n# Refused: exit status 5.\ninvestigation observe 'it is' \"a $B\" \\\n  --x <<'EOF'\n{\"a\": \"$ID\"}\nEOF\n```\n\n```text\nout\n```\n",
            "doc",
            "investigation",
        );
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].exit, 5);
        assert_eq!(steps[0].quote.as_deref(), Some(&["out".to_owned()][..]));
        let Action::Command { argv, stdin } = &steps[0].actions[0] else {
            panic!()
        };
        assert_eq!(argv[0], "observe");
        assert_eq!(argv.last().unwrap(), "--x");
        assert_eq!(argv[1], "it is");
        assert_eq!(
            stdin
                .as_ref()
                .map(|(text, expands)| (text.as_str(), *expands)),
            Some(("{\"a\": \"$ID\"}\n", false))
        );
    }

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn bless_rewrites_a_changed_repair_and_keeps_the_rest_as_written() {
        let quote = lines(&[
            "/operations/0/payload/x: Record it.",
            "Retry or inspect with investigation --workspace <WORKSPACE> show concl_1a2b3c4d5e6f --depth full --format json",
            "See investigation schema conclusion.request_review",
        ]);
        let printed = "/operations/0/payload/x: Record it.\nRetry or inspect with investigation --workspace <WORKSPACE> show concl_1a2b3c4d5e6f --case case_9f8e7d6c5b4a --depth full --format json\nSee investigation schema conclusion.request_review";
        let new = rewrite(&quote, printed, &BTreeMap::new()).unwrap();
        assert_eq!(new[0], quote[0]);
        assert_eq!(new[1], printed.lines().nth(1).unwrap());
        assert_eq!(new[2], quote[2]);
    }

    #[test]
    fn bless_follows_a_renamed_export_folder_into_later_commands() {
        let markdown = "```sh\ninvestigation export\n```\n\n```text\nexported  decision-record  complete  exports/decision-record-case_1-44-16d59b1df19a\n...\n```\n\n```sh\ncat ./demo-case/exports/decision-record-case_1-44-16d59b1df19a/decision-record.md\n```\n";
        let steps = parse(markdown, "doc", "investigation");
        let printed = "exported  decision-record  complete  exports/decision-record-case_1-44-fe655fdab412\nfiles  3";
        let mut session = Session::new(Path::new(".")).bless();
        session.bless_quote(&steps[0], steps[0].quote.as_ref().unwrap(), printed);
        let blessed = blessed(markdown, session.blessing().unwrap());
        assert!(!blessed.contains("16d59b1df19a"), "{blessed}");
        assert_eq!(blessed.matches("fe655fdab412").count(), 2, "{blessed}");
        // The later command reads the renamed folder when it runs.
        let renamed = session.renamed_step(&steps[1]);
        let Action::Read { path } = &renamed.actions[0] else {
            panic!()
        };
        assert!(path.contains("fe655fdab412"), "{path}");
    }

    #[test]
    fn bless_keeps_names_cuts_and_elisions() {
        let mut bound = BTreeMap::new();
        bound.insert("CASE".to_owned(), "case_1".to_owned());
        let quote = lines(&[
            "dry_run  would_result committed ...",
            "$RETRO  revision 1",
            "$CASE  revision 2",
            "...",
        ]);
        let printed = "dry_run  would_result rejected  nothing committed\nretro_77  revision 1\ncase_1  revision 3\nWarning: x";
        let new = rewrite(&quote, printed, &bound).unwrap();
        assert_eq!(
            new,
            lines(&[
                "dry_run  would_result rejected ...",
                "$RETRO  revision 1",
                "$CASE  revision 3",
                "..."
            ])
        );
    }

    #[test]
    fn bless_refuses_to_guess() {
        // A line inserted inside a run anchored at the start.
        let quote = lines(&["alpha one", "beta two", "..."]);
        let error = rewrite(
            &quote,
            "alpha one\ninserted line\nbeta three",
            &BTreeMap::new(),
        );
        assert!(error.is_err(), "{error:?}");
        // A run that fits two places equally well.
        let quote = lines(&["...", "row 1 value", "..."]);
        let error = rewrite(&quote, "row 2 value\nother\nrow 3 value", &BTreeMap::new());
        assert!(error.unwrap_err().contains("equally well"));
        // A bound name whose value is no longer printed.
        let mut bound = BTreeMap::new();
        bound.insert("ID".to_owned(), "art_1".to_owned());
        let quote = lines(&["$ID  revision 1 extra words here"]);
        assert!(rewrite(&quote, "art_2  revision 1 extra words here", &bound).is_err());
    }
}
