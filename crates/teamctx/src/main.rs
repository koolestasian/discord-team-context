//! teamctx command-line program: CLI, configuration and wiring.
//!
//! Pre-alpha: init/doctor currently validate configuration offline; live Discord
//! validation and sync are gated on a throwaway test server.

mod config;
mod eval;
mod token;

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::process::ExitCode;

const VERSION: &str = concat!("teamctx ", env!("CARGO_PKG_VERSION"), "\n");

const HELP: &str = concat!(
    "teamctx ",
    env!("CARGO_PKG_VERSION"),
    " (pre-alpha)\n",
    "\n",
    "Turns a team's Discord into a searchable team record.\n",
    "\n",
    "Offline init and doctor; live Discord support is not available yet.\n",
    "\n",
    "Usage: teamctx [OPTIONS] | init|doctor [OFFLINE OPTIONS]\n",
    "\n",
    "Options:\n",
    "  -h, --help     Print this help\n",
    "  -V, --version  Print the version\n",
    "\nOffline commands:\n",
    "  init          Print a config template and optional bot invite; writes no files\n",
    "  doctor        Validate config and token presence; makes no network calls\n",
    "  token         create|list|revoke access tokens (see 'teamctx token --help')\n",
    "\nOffline options:\n",
    "  --config PATH         TOML file (default teamctx.toml if present)\n",
    "  --guild-id ID         Discord server id (required unless in file/environment)\n",
    "  --application-id ID   Print an invite in init\n",
    "  --bind IP:PORT        Server bind (default 127.0.0.1:7420)\n",
    "  --timezone NAME       Host timezone (default UTC; database not checked)\n",
    "  --public-url URL      HTTP(S) URL used in links\n",
    "  --token-env NAME      Environment variable containing bot token\n",
    "  --include-channels LIST / --exclude-channels LIST (comma-separated)\n",
    "  --backfill-days N / --recheck-window-hours N\n",
);

/// Exit code for a command-line usage error (the same code `clap` will use later).
const EXIT_USAGE: u8 = 2;

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Init(config::OfflineOptions),
    Doctor(config::OfflineOptions),
    Token(token::Command),
    Eval(eval::Options),
}

#[derive(Debug, PartialEq, Eq)]
enum UsageError {
    Unknown(OsString),
    Extra(OsString),
    Config(config::ConfigError),
    Token(token::UsageError),
    Eval(eval::ArgError),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `{:?}` escapes control characters, so an argument cannot mangle the terminal.
        match self {
            Self::Unknown(arg) => write!(f, "unrecognised argument {arg:?}"),
            Self::Extra(arg) => write!(f, "unexpected extra argument {arg:?}"),
            Self::Config(error) => fmt::Display::fmt(error, f),
            Self::Token(error) => fmt::Display::fmt(error, f),
            Self::Eval(error) => fmt::Display::fmt(error, f),
        }
    }
}

/// Accepts help/version flags or an offline command with explicit config options.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let command = match args.next() {
        None => return Ok(Command::Help),
        Some(arg) => match arg.to_str() {
            Some("-h" | "--help") => Command::Help,
            Some("-V" | "--version") => Command::Version,
            Some("init") => {
                return config::parse_cli(args)
                    .map(Command::Init)
                    .map_err(UsageError::Config);
            }
            Some("doctor") => {
                return config::parse_cli(args)
                    .map(Command::Doctor)
                    .map_err(UsageError::Config);
            }
            Some("token") => {
                return token::parse(args)
                    .map(Command::Token)
                    .map_err(UsageError::Token);
            }
            Some("eval") => {
                return eval::parse_args(args)
                    .map(Command::Eval)
                    .map_err(UsageError::Eval);
            }
            _ => return Err(UsageError::Unknown(arg)),
        },
    };
    match args.next() {
        None => Ok(command),
        Some(extra) => Err(UsageError::Extra(extra)),
    }
}

/// Runs the program and returns the process exit code.
fn run(args: impl IntoIterator<Item = OsString>, out: &mut impl Write, err: &mut impl Write) -> u8 {
    let written = match parse(args) {
        Ok(Command::Help) => out.write_all(HELP.as_bytes()),
        Ok(Command::Version) => out.write_all(VERSION.as_bytes()),
        Ok(Command::Init(options)) => return run_offline(true, &options, out, err),
        Ok(Command::Doctor(options)) => return run_offline(false, &options, out, err),
        Ok(Command::Token(command)) => return token::run(&command, out, err),
        Ok(Command::Eval(options)) => return run_eval(&options, out, err),
        Err(error) => {
            // If stderr is gone there is nowhere left to report that, so the error is dropped.
            let _ = writeln!(err, "error: {error}\nTry 'teamctx --help'.");
            return EXIT_USAGE;
        }
    };
    u8::from(written.is_err())
}

fn run_offline(
    init: bool,
    options: &config::OfflineOptions,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    match config::execute(init, options, out) {
        Ok(success) => u8::from(!success),
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            1
        }
    }
}

fn run_eval(options: &eval::Options, out: &mut impl Write, err: &mut impl Write) -> u8 {
    match eval::execute(options, out) {
        Ok(success) => u8::from(!success),
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            1
        }
    }
}

fn main() -> ExitCode {
    let code = run(
        std::env::args_os().skip(1),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    );
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn known_flags_parse() {
        assert_eq!(parse_strs(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["-h"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["--version"]), Ok(Command::Version));
        assert_eq!(parse_strs(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn no_arguments_prints_help() {
        assert_eq!(parse_strs(&[]), Ok(Command::Help));
    }

    #[test]
    fn unknown_and_extra_arguments_are_errors() {
        assert_eq!(
            parse_strs(&["serve"]),
            Err(UsageError::Unknown(OsString::from("serve")))
        );
        assert_eq!(
            parse_strs(&["--version", "--help"]),
            Err(UsageError::Extra(OsString::from("--help")))
        );
    }

    #[test]
    fn usage_error_escapes_control_characters() {
        let message = UsageError::Unknown(OsString::from("a\u{1b}[31mb")).to_string();
        assert!(!message.contains('\u{1b}'), "{message:?}");
    }

    #[test]
    fn run_reports_usage_errors_on_stderr_only() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run([OsString::from("nope")], &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert!(String::from_utf8_lossy(&err).starts_with("error: "));
    }
}
