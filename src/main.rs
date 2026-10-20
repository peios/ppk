use std::error::Error as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Parser, Subcommand};
use ppk::PackageId;

/// Exit status for a problem with the arguments, as distinct from a failure.
const EXIT_USAGE: u8 = 2;

/// Build and manage Peios packages
#[derive(Parser)]
#[command(
    name = "ppk",
    version,
    arg_required_else_help = true,
    subcommand_value_name = "command"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pack a directory into the artifact directory
    #[command(override_usage = "ppk pack [options] <package> <path>")]
    Pack {
        /// Package to create, as <name>@<version>[?arch=<arch>]
        #[arg(value_name = "package")]
        package: PackageId,
        /// Directory whose contents become the package's files
        #[arg(value_name = "path")]
        path: PathBuf,
        /// Store artifacts in this directory (default: /system/artifacts)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::DEFAULT_ARTIFACT_DIR,
            hide_default_value = true,
        )]
        artifact_dir: PathBuf,
    },
}

fn main() -> ExitCode {
    let program = program_name();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return report_parse_error(&program, &err),
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{program}: {}", describe(&err));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Pack {
            package,
            path,
            artifact_dir,
        } => {
            let packed = ppk::pack(&package, &path, &artifact_dir)?;
            println!("{}", packed.identity);
        }
    }
    Ok(())
}

/// The name ppk was invoked as, without its directory.
fn program_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|arg0| {
            Path::new(&arg0)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "ppk".to_owned())
}

/// Print what clap stopped on and choose the exit status. Requested help and
/// version go to stdout; help shown because nothing was given is a usage
/// error and goes to stderr; every other error becomes one `ppk: ` line.
fn report_parse_error(program: &str, err: &clap::Error) -> ExitCode {
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            return match err.print() {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            };
        }
        ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            eprint!("{}", err.render());
            return ExitCode::from(EXIT_USAGE);
        }
        _ => {}
    }
    eprintln!("{program}: {}", usage_message(err));
    eprintln!("Try '{program} --help' for more information.");
    ExitCode::from(EXIT_USAGE)
}

/// clap's own wording for a usage error, reworded where it differs from the
/// Peios form and with its `error: ` prefix, usage and tips removed.
fn usage_message(err: &clap::Error) -> String {
    let args: Vec<&str> = match err.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(arg)) => vec![arg.as_str()],
        Some(ContextValue::Strings(args)) => args.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    };
    match (err.kind(), args.as_slice()) {
        (ErrorKind::UnknownArgument, [arg]) if arg.starts_with('-') => {
            format!("unknown option '{arg}'")
        }
        (ErrorKind::UnknownArgument, [arg]) => format!("unexpected argument '{arg}'"),
        (ErrorKind::MissingRequiredArgument, [_, ..]) => {
            format!("missing argument {}", args.join(" "))
        }
        (ErrorKind::ValueValidation, _) if err.source().is_some() => {
            err.source().map(ToString::to_string).unwrap_or_default()
        }
        _ => {
            let rendered = err.render().to_string();
            let first = rendered.lines().next().unwrap_or_default();
            first.strip_prefix("error: ").unwrap_or(first).to_owned()
        }
    }
}

/// The error and its causes on one line, outermost first, without the
/// `(os error N)` that Rust appends to system errors.
fn describe(err: &anyhow::Error) -> String {
    let causes: Vec<String> = err
        .chain()
        .map(|cause| {
            let text = cause.to_string();
            match text.rfind(" (os error ") {
                Some(at) if text.ends_with(')') => text[..at].to_owned(),
                _ => text,
            }
        })
        .collect();
    causes.join(": ")
}
