//! Developer tasks for the teamctx workspace, run as `cargo xtask <task>`.
//!
//! The `xtask` alias lives in `.cargo/config.toml`. This crate uses only the standard
//! library; keep it that way.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

const HELP: &str = "\
Developer tasks for the teamctx workspace.

Usage: cargo xtask <TASK>

Tasks:
  help  Print this list

There are no other tasks yet. `eval` (retrieval quality) arrives with milestone M2.
";

/// Exit code for an unknown task or bad arguments.
const EXIT_USAGE: u8 = 2;

/// Runs the requested task and returns the process exit code.
fn run(args: &[OsString], out: &mut impl Write, err: &mut impl Write) -> u8 {
    match args {
        [] => u8::from(out.write_all(HELP.as_bytes()).is_err()),
        [task] if task == "help" || task == "--help" || task == "-h" => {
            u8::from(out.write_all(HELP.as_bytes()).is_err())
        }
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
