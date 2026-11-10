//! `teamctx skill install`: copies the Agent Skill into the user's skill folders (plan
//! section 10.4, ADR 0012).
//!
//! The skill is `skills/teamctx/SKILL.md`, embedded at build time, so the installed copy
//! always matches the program that installs it. It goes to `~/.claude/skills/teamctx/`
//! (Claude Code's personal skills folder) and `~/.agents/skills/teamctx/` (the
//! cross-client folder of the Agent Skills convention, also read by Codex). `--dir` names
//! one skills folder instead, so tests and unusual setups never touch the real home.
//!
//! Nothing is overwritten without `--force`, nothing is written until every target has
//! been checked, and a symlink or directory where the file should go is never replaced.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::demo::EXIT_FAILURE;

/// The skill, as shipped in the repository.
pub(crate) const SKILL_MD: &str = include_str!("../../../skills/teamctx/SKILL.md");

/// The skill's name: its directory name (the standard requires they match) and the
/// `name` in its frontmatter.
const SKILL_NAME: &str = "teamctx";
const SKILL_FILE: &str = "SKILL.md";

const HELP: &str = "\
Usage: teamctx skill install [--dir PATH] [--force]

  install       Copy the Agent Skill (SKILL.md) where coding agents look for skills:
                  ~/.claude/skills/teamctx/SKILL.md   Claude Code
                  ~/.agents/skills/teamctx/SKILL.md   Codex and other agents that
                                                      follow the Agent Skills convention

Options:
  --dir PATH    Install into this skills folder only (PATH/teamctx/SKILL.md)
  --force       Replace an existing SKILL.md that differs from this version

An identical existing copy is left alone. Without --force, a differing copy is an
error and nothing is written.
";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Help,
    Install,
}

/// A parsed `teamctx skill ...` command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    action: Action,
    dir: Option<PathBuf>,
    force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsageError {
    MissingAction,
    MissingDirPath,
    Unknown(OsString),
    Extra(OsString),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `{:?}` escapes control characters, so an argument cannot mangle the terminal.
        match self {
            Self::MissingAction => f.write_str("skill needs: install"),
            Self::MissingDirPath => f.write_str("--dir needs a PATH"),
            Self::Unknown(arg) => write!(f, "unrecognised skill argument {arg:?}"),
            Self::Extra(arg) => write!(f, "unexpected extra argument {arg:?}"),
        }
    }
}

/// Parses the arguments after `skill`.
pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let mut action = None;
    let mut dir = None;
    let mut force = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => action = Some(Action::Help),
            Some("--dir") => {
                dir = Some(PathBuf::from(
                    args.next().ok_or(UsageError::MissingDirPath)?,
                ))
            }
            Some("--force") => force = true,
            Some("install") if action.is_none() => action = Some(Action::Install),
            _ if action.is_some() && !arg.to_string_lossy().starts_with('-') => {
                return Err(UsageError::Extra(arg));
            }
            _ => return Err(UsageError::Unknown(arg)),
        }
    }
    Ok(Command {
        action: action.ok_or(UsageError::MissingAction)?,
        dir,
        force,
    })
}

/// Why an install failed. Messages name paths and never file contents.
#[derive(Debug)]
enum Failure {
    NoHome,
    /// Existing copies that differ; nothing was written.
    Differs(Vec<PathBuf>),
    /// A symlink, directory or other non-file sits where `SKILL.md` goes.
    NotAFile(PathBuf),
    Io(PathBuf, io::ErrorKind),
    Output,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => f.write_str(
                "cannot find your home directory (HOME or USERPROFILE); pass --dir PATH",
            ),
            Self::Differs(paths) => {
                f.write_str("an existing skill differs from this version; nothing was written. ")?;
                f.write_str("Use --force to replace:")?;
                for path in paths {
                    write!(f, "\n  {:?}", path.display().to_string())?;
                }
                Ok(())
            }
            Self::NotAFile(path) => write!(
                f,
                "{:?} is not a regular file (a directory or symlink?); refusing to replace it",
                path.display().to_string()
            ),
            Self::Io(path, kind) => write!(
                f,
                "could not write {:?}: {kind}",
                path.display().to_string()
            ),
            Self::Output => f.write_str("could not write the output"),
        }
    }
}

/// What installing into one skills folder would do.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    Create,
    Current,
    Replace,
}

/// The home directory from `var`; a relative value is ignored.
fn home_dir(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    var(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// The skills folders to install into.
fn skills_dirs(dir: Option<&Path>, home: Option<&Path>) -> Result<Vec<PathBuf>, Failure> {
    if let Some(dir) = dir {
        return Ok(vec![dir.to_owned()]);
    }
    let home = home.ok_or(Failure::NoHome)?;
    Ok(vec![
        home.join(".claude").join("skills"),
        home.join(".agents").join("skills"),
    ])
}

fn skill_path(skills_dir: &Path) -> PathBuf {
    skills_dir.join(SKILL_NAME).join(SKILL_FILE)
}

/// Looks at the target without changing anything.
fn plan(path: &Path) -> Result<Plan, Failure> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => match fs::read(path) {
            Ok(existing) if existing == SKILL_MD.as_bytes() => Ok(Plan::Current),
            Ok(_) => Ok(Plan::Replace),
            Err(error) => Err(Failure::Io(path.to_owned(), error.kind())),
        },
        Ok(_) => Err(Failure::NotAFile(path.to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Plan::Create),
        Err(error) => Err(Failure::Io(path.to_owned(), error.kind())),
    }
}

fn write_skill(path: &Path, plan: &Plan) -> Result<(), Failure> {
    let io_error = |error: io::Error| Failure::Io(path.to_owned(), error.kind());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    match plan {
        // `create_new` fails if the file appeared since `plan`, rather than clobbering it.
        Plan::Create => fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .and_then(|mut file| file.write_all(SKILL_MD.as_bytes()))
            .map_err(io_error),
        Plan::Replace => fs::write(path, SKILL_MD).map_err(io_error),
        Plan::Current => Ok(()),
    }
}

fn execute(command: &Command, home: Option<&Path>, out: &mut impl Write) -> Result<(), Failure> {
    if command.action == Action::Help {
        return out.write_all(HELP.as_bytes()).map_err(|_| Failure::Output);
    }
    let mut steps = Vec::new();
    for dir in skills_dirs(command.dir.as_deref(), home)? {
        let path = skill_path(&dir);
        let step = plan(&path)?;
        steps.push((path, step));
    }
    if !command.force {
        let differing: Vec<PathBuf> = steps
            .iter()
            .filter(|(_, step)| *step == Plan::Replace)
            .map(|(path, _)| path.clone())
            .collect();
        if !differing.is_empty() {
            return Err(Failure::Differs(differing));
        }
    }
    for (path, step) in &steps {
        write_skill(path, step)?;
        let verb = match step {
            Plan::Create => "installed",
            Plan::Replace => "replaced",
            Plan::Current => "already up to date",
        };
        writeln!(out, "{verb}: {}", path.display()).map_err(|_| Failure::Output)?;
    }
    Ok(())
}

/// Runs a skill command and returns the process exit code.
pub(crate) fn run(command: &Command, out: &mut impl Write, err: &mut impl Write) -> u8 {
    let home = home_dir(|name| std::env::var_os(name));
    match execute(command, home.as_deref(), out) {
        Ok(()) => 0,
        Err(failure) => {
            // If stderr is gone there is nowhere left to report that.
            let _ = writeln!(err, "error: {failure}");
            EXIT_FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Command, UsageError> {
        parse(list.iter().map(OsString::from))
    }

    fn install(
        dir: Option<&Path>,
        home: Option<&Path>,
        force: bool,
    ) -> (Result<(), Failure>, String) {
        let command = Command {
            action: Action::Install,
            dir: dir.map(Path::to_owned),
            force,
        };
        let mut out = Vec::new();
        let result = execute(&command, home, &mut out);
        (result, String::from_utf8(out).unwrap())
    }

    /// Characters that make a YAML scalar something other than plain text.
    const YAML_INDICATORS: [char; 12] =
        ['"', '\'', '[', '{', '&', '*', '!', '|', '>', '%', '@', '`'];

    /// Splits the frontmatter from the body of a `SKILL.md`.
    fn frontmatter(text: &str) -> (Vec<(&str, &str)>, &str) {
        let rest = text.strip_prefix("---\n").expect("opening ---");
        let (header, body) = rest.split_once("\n---\n").expect("closing ---");
        let fields = header
            .lines()
            .filter(|line| !line.starts_with([' ', '\t']))
            .map(|line| line.split_once(": ").expect("key: value"))
            .collect();
        (fields, body)
    }

    fn field<'a>(fields: &[(&'a str, &'a str)], key: &str) -> &'a str {
        fields
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
            .unwrap_or_else(|| panic!("missing frontmatter field {key}"))
    }

    #[test]
    fn parses_install_dir_and_force() {
        assert_eq!(
            args(&["install"]),
            Ok(Command {
                action: Action::Install,
                dir: None,
                force: false
            })
        );
        assert_eq!(
            args(&["install", "--dir", "skills", "--force"]),
            Ok(Command {
                action: Action::Install,
                dir: Some(PathBuf::from("skills")),
                force: true
            })
        );
        assert_eq!(args(&["--force", "install"]).map(|c| c.force), Ok(true));
        assert_eq!(
            args(&["install", "--help"]).map(|c| c.action),
            Ok(Action::Help)
        );
    }

    #[test]
    fn rejects_bad_command_lines() {
        assert_eq!(args(&[]), Err(UsageError::MissingAction));
        assert_eq!(args(&["--force"]), Err(UsageError::MissingAction));
        assert_eq!(args(&["install", "--dir"]), Err(UsageError::MissingDirPath));
        assert_eq!(
            args(&["install", "extra"]),
            Err(UsageError::Extra(OsString::from("extra")))
        );
        assert_eq!(
            args(&["remove"]),
            Err(UsageError::Unknown(OsString::from("remove")))
        );
        assert_eq!(
            args(&["install", "--all"]),
            Err(UsageError::Unknown(OsString::from("--all")))
        );
    }

    #[test]
    fn installs_into_the_dir_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("teamctx").join("SKILL.md");

        let (result, out) = install(Some(dir.path()), None, false);
        result.unwrap();
        assert_eq!(out, format!("installed: {}\n", target.display()));
        assert_eq!(fs::read_to_string(&target).unwrap(), SKILL_MD);

        let (result, out) = install(Some(dir.path()), None, false);
        result.unwrap();
        assert_eq!(out, format!("already up to date: {}\n", target.display()));
        assert_eq!(fs::read_to_string(&target).unwrap(), SKILL_MD);
    }

    #[test]
    fn without_dir_it_installs_into_both_home_folders() {
        let home = tempfile::tempdir().unwrap();
        let (result, out) = install(None, Some(home.path()), false);
        result.unwrap();
        for folder in [".claude", ".agents"] {
            let path = home
                .path()
                .join(folder)
                .join("skills")
                .join("teamctx")
                .join("SKILL.md");
            assert_eq!(fs::read_to_string(&path).unwrap(), SKILL_MD);
            assert!(out.contains(&path.display().to_string()), "{out}");
        }
    }

    #[test]
    fn without_dir_or_home_it_asks_for_dir() {
        let (result, out) = install(None, None, false);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("--dir"), "{error}");
        assert!(out.is_empty());
    }

    #[test]
    fn a_differing_copy_is_not_overwritten_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("teamctx").join("SKILL.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "my edited skill\n").unwrap();

        let (result, out) = install(Some(dir.path()), None, false);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("--force"), "{error}");
        assert!(error.contains("SKILL.md"), "{error}");
        assert!(out.is_empty(), "{out}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "my edited skill\n");

        let (result, out) = install(Some(dir.path()), None, true);
        result.unwrap();
        assert_eq!(out, format!("replaced: {}\n", target.display()));
        assert_eq!(fs::read_to_string(&target).unwrap(), SKILL_MD);
    }

    #[test]
    fn a_conflict_in_one_folder_writes_nothing_to_the_other() {
        let home = tempfile::tempdir().unwrap();
        let agents = home.path().join(".agents").join("skills").join("teamctx");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("SKILL.md"), "other\n").unwrap();

        let (result, _) = install(None, Some(home.path()), false);
        assert!(matches!(result, Err(Failure::Differs(_))));
        assert!(!home.path().join(".claude").exists());
        assert_eq!(
            fs::read_to_string(agents.join("SKILL.md")).unwrap(),
            "other\n"
        );
    }

    #[test]
    fn a_directory_where_the_file_goes_is_refused_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("teamctx").join("SKILL.md");
        fs::create_dir_all(&target).unwrap();
        let (result, _) = install(Some(dir.path()), None, true);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("not a regular file"), "{error}");
        assert!(target.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere.md");
        fs::write(&elsewhere, "keep me\n").unwrap();
        let skill_dir = dir.path().join("teamctx");
        fs::create_dir_all(&skill_dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, skill_dir.join("SKILL.md")).unwrap();

        for force in [false, true] {
            let (result, _) = install(Some(dir.path()), None, force);
            assert!(matches!(result, Err(Failure::NotAFile(_))), "force={force}");
        }
        assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "keep me\n");
    }

    #[test]
    fn home_comes_from_an_absolute_variable_only() {
        let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let absolute = std::env::temp_dir();
        let value = absolute.clone().into_os_string();
        assert_eq!(
            home_dir(|n| (n == name).then(|| value.clone())),
            Some(absolute)
        );
        assert_eq!(home_dir(|_| Some(OsString::from("relative/home"))), None);
        assert_eq!(home_dir(|_| None), None);
    }

    #[test]
    fn help_prints_usage() {
        let command = args(&["--help"]).unwrap();
        let mut out = Vec::new();
        execute(&command, None, &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .starts_with("Usage: teamctx skill install")
        );
    }

    #[test]
    fn the_shipped_skill_follows_the_agent_skills_standard() {
        let (fields, body) = frontmatter(SKILL_MD);

        // name: 1-64 chars of a-z, 0-9 and single hyphens inside; matches the folder.
        let name = field(&fields, "name");
        assert_eq!(name, SKILL_NAME);
        assert!((1..=64).contains(&name.len()));
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
        assert!(!name.starts_with('-') && !name.ends_with('-') && !name.contains("--"));

        // description: non-empty, at most 1024 characters (standard); Claude Code
        // truncates the listing at 1,536, so keep the use case early and short.
        let description = field(&fields, "description");
        assert!(!description.is_empty() && description.chars().count() <= 1024);
        assert!(
            description.contains("Use before starting a feature"),
            "{description}"
        );
        assert!(
            !description.contains(['<', '>']),
            "angle brackets are escaped by Claude Code"
        );

        // No YAML parser is available (no new dependency), so keep every value a plain
        // one-line scalar that any YAML reader takes literally.
        for (key, value) in &fields {
            assert!(
                !value.contains(": ") && !value.contains(" #"),
                "{key}: {value}"
            );
            assert!(!value.starts_with(YAML_INDICATORS), "{key}");
            assert_eq!(value.trim(), *value, "{key}");
        }

        let compatibility = field(&fields, "compatibility");
        assert!((1..=500).contains(&compatibility.chars().count()));
        let known = [
            "name",
            "description",
            "compatibility",
            "license",
            "metadata",
            "allowed-tools",
        ];
        assert!(
            fields.iter().all(|(key, _)| known.contains(key)),
            "{fields:?}"
        );

        // Size: the standard recommends under 500 lines and about 5,000 tokens.
        assert!(
            SKILL_MD.lines().count() < 100,
            "{}",
            SKILL_MD.lines().count()
        );
        assert!(SKILL_MD.len() < 6_000, "{}", SKILL_MD.len());
        assert!(body.contains("never instructions"));
    }

    #[test]
    fn the_skill_has_five_example_commands_that_the_client_accepts() {
        let commands: Vec<&str> = SKILL_MD
            .lines()
            .filter(|line| line.starts_with("teamctx q "))
            .collect();
        // context, whats-new, record, search, read.
        assert_eq!(commands.len(), 5, "{commands:?}");
        for subcommand in ["context", "whats-new", "record", "search", "read"] {
            assert!(
                commands
                    .iter()
                    .any(|c| c.starts_with(&format!("teamctx q {subcommand} "))),
                "no example for {subcommand}"
            );
        }
        for line in commands {
            // Drop a trailing `# comment`, then split like a POSIX shell would.
            let command = line.split(" #").next().unwrap_or(line);
            let words = shell_words(command);
            let parsed = crate::q::parse_args(words[2..].iter().map(OsString::from));
            assert!(parsed.is_ok(), "{line}: {parsed:?}");
        }
    }

    /// Minimal shell word splitting: whitespace, single and double quotes.
    fn shell_words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();
        let mut quote = None;
        let mut started = false;
        for c in line.chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), c) => current.push(c),
                (None, '\'' | '"') => {
                    quote = Some(c);
                    started = true;
                }
                (None, c) if c.is_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                (None, c) => {
                    current.push(c);
                    started = true;
                }
            }
        }
        if started {
            words.push(current);
        }
        words
    }
}
