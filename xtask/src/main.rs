//! Developer tasks for the teamctx workspace, run as `cargo xtask <task>`.
//!
//! The `xtask` alias lives in `.cargo/config.toml`. This crate uses only the standard
//! library; keep it that way. Tasks with real logic live in the `teamctx` binary and are
//! only launched from here.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::{Command, ExitCode};

const HELP: &str = "\
Developer tasks for the teamctx workspace.

Usage: cargo xtask <TASK> [ARGS]

Tasks:
  eval  Measure retrieval quality on the demo dataset (recall@10, MRR).
        Runs `teamctx eval`; arguments after the task name are passed on:
        `cargo xtask eval -- --check` fails if quality fell below the baseline,
        `cargo xtask eval -- --write-baseline` rewrites fixtures/demo/eval-baseline.toml.
  help  Print this list
";

/// Exit code for an unknown task or bad arguments.
const EXIT_USAGE: u8 = 2;

/// The `cargo` arguments that run `teamctx eval` with the user's arguments. A leading
/// `--` (as in `cargo xtask eval -- --check`) is dropped so the flags reach `teamctx`.
fn eval_cargo_args(rest: &[OsString]) -> Vec<OsString> {
    let rest = match rest {
        [first, tail @ ..] if first == "--" => tail,
        other => other,
    };
    ["run", "-q", "-p", "teamctx", "--", "eval"]
        .iter()
        .map(OsString::from)
        .chain(rest.iter().cloned())
        .collect()
}

/// Runs `teamctx eval` through cargo and forwards its exit code.
fn run_eval(rest: &[OsString], err: &mut impl Write) -> u8 {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    match Command::new(cargo).args(eval_cargo_args(rest)).status() {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1),
        Err(error) => {
            let _ = writeln!(err, "error: cannot run cargo: {error}");
            1
        }
    }
}

/// Runs the requested task and returns the process exit code.
fn run(args: &[OsString], out: &mut impl Write, err: &mut impl Write) -> u8 {
    match args {
        [] => u8::from(out.write_all(HELP.as_bytes()).is_err()),
        [task] if task == "help" || task == "--help" || task == "-h" => {
            u8::from(out.write_all(HELP.as_bytes()).is_err())
        }
        [task, rest @ ..] if task == "eval" => run_eval(rest, err),
        [task, ..] => {
            // `{:?}` escapes control characters in what the user typed.
            let _ = writeln!(err, "error: unknown task {task:?}\n\n{HELP}");
            EXIT_USAGE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    ExitCode::from(run(
        &args,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn eval_runs_the_teamctx_subcommand_and_forwards_flags() {
        let prefix = ["run", "-q", "-p", "teamctx", "--", "eval"];
        for (given, forwarded) in [
            (&[][..], &[][..]),
            (&["--", "--check"], &["--check"]),
            (&["--check"], &["--check"]),
            (&["--", "--write-baseline"], &["--write-baseline"]),
            // Only the first `--` is the separator.
            (&["--", "--", "x"], &["--", "x"]),
        ] {
            let mut expected = os(&prefix);
            expected.extend(os(forwarded));
            assert_eq!(eval_cargo_args(&os(given)), expected, "{given:?}");
        }
    }
}
