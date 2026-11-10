//! `teamctx export --format okf|md <dir>`: writes the team record as plain files
//! (plan sections 10.3 and 10.5, ADR 0013).
//!
//! - `okf`: an [OKF 0.2](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md)
//!   bundle: `index.md`, `log.md`, and one file per entry under `decisions/`, `tasks/` and
//!   `questions/` ([`okf`]).
//! - `md`: one readable `record.md` ([`md`]).
//!
//! Safety rules:
//! - The record is read through a read-only connection and rendered completely in memory
//!   before the first file is written, so a rendering problem never leaves half a bundle.
//! - Chat text is untrusted. It reaches file names only as a `[a-z0-9-]` slug, YAML only as
//!   an escaped double-quoted string, and Markdown only escaped ([`text`]).
//! - A directory that already has anything in it is refused. `--force` replaces only the
//!   files this command writes (by exact name pattern) and never follows a symbolic link
//!   out of the directory. Files are created with `create_new`, so an existing file or
//!   link is never written through.
//! - No message text, token or configuration value is exported: only titles, names, ids,
//!   dates and Discord jump links.

mod md;
mod model;
mod okf;
mod text;

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use teamctx_store::Store;

use crate::demo::{EXIT_FAILURE, EXIT_USAGE, default_db_from_env, take_value};

/// One file of an export, with a path relative to the output directory using `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct File {
    pub(crate) path: String,
    pub(crate) content: String,
}

/// The export layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// An OKF 0.2 bundle.
    Okf,
    /// A single `record.md`.
    Md,
}

/// Options of `teamctx export`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Options {
    format: Format,
    dir: PathBuf,
    db: Option<PathBuf>,
    force: bool,
}

/// A mistake in the arguments of `export`. Values are untrusted and shown escaped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ArgError {
    UnknownOption(OsString),
    MissingValue(&'static str),
    MissingFormat,
    BadFormat,
    MissingDir,
    ExtraArgument(OsString),
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption(arg) => write!(f, "unrecognised argument {arg:?}"),
            Self::MissingValue(option) => write!(f, "{option} needs a value"),
            Self::MissingFormat => f.write_str("export needs --format okf or --format md"),
            Self::BadFormat => f.write_str("--format must be okf or md"),
            Self::MissingDir => f.write_str(
                "export needs an output directory, for example: teamctx export --format okf record",
            ),
            Self::ExtraArgument(arg) => write!(f, "unexpected extra argument {arg:?}"),
        }
    }
}

/// Parses `--format okf|md <dir> [--db PATH] [--force]`, in any order.
pub(crate) fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Options, ArgError> {
    let mut format = None;
    let mut dir: Option<PathBuf> = None;
    let mut db = None;
    let mut force = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--format") => {
                let value = take_value(&mut args, "--format")
                    .map_err(|_| ArgError::MissingValue("--format"))?;
                format = Some(match value.to_str() {
                    Some("okf") => Format::Okf,
                    Some("md") => Format::Md,
                    _ => return Err(ArgError::BadFormat),
                });
            }
            Some("--db") => {
                db = Some(
                    take_value(&mut args, "--db")
                        .map_err(|_| ArgError::MissingValue("--db"))?
                        .into(),
                );
            }
            Some("--force") => force = true,
            Some(flag) if flag.starts_with('-') => return Err(ArgError::UnknownOption(arg)),
            _ => {
                if dir.is_some() {
                    return Err(ArgError::ExtraArgument(arg));
                }
                dir = Some(arg.into());
            }
        }
    }
    Ok(Options {
        format: format.ok_or(ArgError::MissingFormat)?,
        dir: dir.ok_or(ArgError::MissingDir)?,
        db,
        force,
    })
}

/// Why an export could not be written.
#[derive(Debug)]
pub(crate) enum WriteError {
    /// The output path exists and is not a directory.
    NotADirectory,
    /// The directory has content and `--force` was not given.
    NotEmpty,
    /// A generated path or an existing managed directory is not safe to write to.
    Unsafe(&'static str),
    Io(io::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADirectory => f.write_str("the output path exists and is not a directory"),
            Self::NotEmpty => f.write_str(
                "the output directory is not empty, so nothing was written; \
                 pass --force to replace the files a previous export wrote, or choose another directory",
            ),
            Self::Unsafe(why) => write!(f, "refusing to write: {why}"),
            Self::Io(error) => write!(f, "cannot write the export: {error}"),
        }
    }
}

impl From<io::Error> for WriteError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The file names this command writes at the top of the output directory.
const TOP_LEVEL: [&str; 3] = ["index.md", "log.md", md::FILE_NAME];

/// Whether `name` has the shape of an entry file this command writes:
/// `D-12.md` or `D-12-some-slug.md` (also `T-` and `Q-`).
fn is_entry_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let mut parts = stem.splitn(3, '-');
    let (Some(prefix), Some(number)) = (parts.next(), parts.next()) else {
        return false;
    };
    let slug_ok = parts.next().is_none_or(|slug| {
        !slug.is_empty()
            && slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    matches!(prefix, "D" | "T" | "Q")
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && slug_ok
}

/// Checks that a generated relative path stays inside the output directory: one or two
/// `/`-separated components of `[A-Za-z0-9._-]`, none starting with a dot, and a first
/// component that is one of the managed directories when there are two.
fn check_relative(path: &str) -> Result<(), WriteError> {
    let parts: Vec<&str> = path.split('/').collect();
    let component_ok = |part: &str| {
        !part.is_empty()
            && !part.starts_with('.')
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    let ok = match parts.as_slice() {
        [name] => component_ok(name),
        [dir, name] => okf::directories().contains(dir) && component_ok(name),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(WriteError::Unsafe(
            "a generated path is outside the bundle layout",
        ))
    }
}

/// Removes the files a previous export wrote, and nothing else.
fn clear_managed(dir: &Path) -> Result<(), WriteError> {
    for name in TOP_LEVEL {
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_dir() => fs::remove_file(&path)?,
            Ok(_) => {
                return Err(WriteError::Unsafe(
                    "index.md, log.md or record.md is a directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for sub in okf::directories() {
        let sub_path = dir.join(sub);
        match fs::symlink_metadata(&sub_path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(WriteError::Unsafe(
                    "an entry folder is not a plain directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        for item in fs::read_dir(&sub_path)? {
            let item = item?;
            let is_managed = item.file_name().to_str().is_some_and(is_entry_file_name);
            if is_managed && !item.file_type()?.is_dir() {
                fs::remove_file(item.path())?;
            }
        }
    }
    Ok(())
}

/// Writes `files` into `dir`. See the module docs for the safety rules.
///
/// # Errors
///
/// [`WriteError`] when the directory is not empty (and `force` is off), is not a directory,
/// would be written through a link, or a write fails. Paths are checked before anything is
/// removed or written.
pub(crate) fn write_files(dir: &Path, files: &[File], force: bool) -> Result<(), WriteError> {
    for file in files {
        check_relative(&file.path)?;
    }
    match fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(WriteError::NotADirectory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(dir)?,
        Err(error) => return Err(error.into()),
    }
    if fs::read_dir(dir)?.next().is_some() {
        if !force {
            return Err(WriteError::NotEmpty);
        }
        clear_managed(dir)?;
    }
    for file in files {
        let target = dir.join(&file.path);
        if let Some(parent) = target.parent().filter(|parent| *parent != dir) {
            match fs::symlink_metadata(parent) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(WriteError::Unsafe(
                        "an entry folder is not a plain directory",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(parent)?,
                Err(error) => return Err(error.into()),
            }
        }
        let mut handle = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        handle.write_all(file.content.as_bytes())?;
    }
    Ok(())
}

/// Renders the record in the chosen layout.
fn render(format: Format, snapshot: &model::Snapshot) -> Vec<File> {
    match format {
        Format::Okf => okf::render(snapshot),
        Format::Md => md::render(snapshot),
    }
}

/// Runs `teamctx export` and returns the exit code.
pub(crate) fn run(options: &Options, out: &mut impl Write, err: &mut impl Write) -> u8 {
    let db = options.db.clone().unwrap_or_else(default_db_from_env);
    if !db.is_file() {
        let _ = writeln!(
            err,
            "error: no database at {}; run 'teamctx demo' or pass --db PATH",
            db.display()
        );
        return EXIT_USAGE;
    }
    let snapshot = match Store::open_read_only(&db)
        .map_err(model::GatherError::from)
        .and_then(|store| model::gather(&store))
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            return EXIT_FAILURE;
        }
    };
    let files = render(options.format, &snapshot);
    if let Err(error) = write_files(&options.dir, &files, options.force) {
        let _ = writeln!(err, "error: {error}");
        return match error {
            WriteError::NotEmpty | WriteError::NotADirectory => EXIT_USAGE,
            WriteError::Unsafe(_) | WriteError::Io(_) => EXIT_FAILURE,
        };
    }
    let layout = match options.format {
        Format::Okf => "OKF 0.2 bundle",
        Format::Md => "Markdown file",
    };
    let written = writeln!(
        out,
        "Wrote the record ({} entries, {} files, {layout}) to {}",
        snapshot.items.len(),
        files.len(),
        options.dir.display()
    );
    u8::from(written.is_err())
}

#[cfg(test)]
mod tests;
