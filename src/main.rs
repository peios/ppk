use std::collections::BTreeMap;
use std::error::Error as _;
use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand};
use ppk::{Constraint, LicenseClass, Metadata, PackageId, Version};

/// Exit status for a failure.
const EXIT_FAILURE: u8 = 1;

/// Exit status for a problem with the arguments, as distinct from a failure.
const EXIT_USAGE: u8 = 2;

/// Exit status when `run` finds its command but cannot execute it, as shells
/// use.
const EXIT_CANNOT_EXECUTE: u8 = 126;

/// Exit status when `run` cannot find its command, as shells use.
const EXIT_NOT_FOUND: u8 = 127;

/// The help's closing pointer to the full documentation.
const SEE_ALSO: &str = "See ppk(1).";

/// PSC keeps help within 80 columns; clap wraps to the terminal up to this.
const HELP_WIDTH: usize = 80;

/// Create and manage Peios packages
#[derive(Parser)]
#[command(
    name = "ppk",
    version,
    arg_required_else_help = true,
    disable_help_subcommand = true,
    max_term_width = HELP_WIDTH,
    subcommand_value_name = "command",
    after_help = SEE_ALSO
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pack a directory into the artifact directory
    #[command(override_usage = "ppk pack [options] <package> <path>", after_help = SEE_ALSO)]
    Pack {
        /// Package to create, as <name>@<version>[?arch=<arch>]
        #[arg(value_name = "package")]
        package: PackageId,
        /// Directory whose contents become the package's files under /usr
        #[arg(value_name = "path")]
        path: PathBuf,
        /// Store packages in <dir> (default: /system/artifacts)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::DEFAULT_ARTIFACT_DIR,
            hide_default_value = true,
        )]
        artifact_dir: PathBuf,
        #[command(flatten)]
        metadata: Box<MetadataArgs>,
    },
    /// Copy a package from another artifact directory
    #[command(
        override_usage = "ppk fetch [options] --from <dir> <package>",
        after_help = SEE_ALSO
    )]
    Fetch {
        /// Package to copy, as <name>@<version>[?arch=<arch>]
        #[arg(value_name = "package")]
        package: PackageId,
        /// Store packages in <dir> (default: /system/artifacts)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::DEFAULT_ARTIFACT_DIR,
            hide_default_value = true,
        )]
        artifact_dir: PathBuf,
        /// Copy from the artifact directory <dir>
        #[arg(long, value_name = "dir")]
        from: PathBuf,
    },
    /// Compose packages into a closure
    #[command(
        override_usage = "ppk closure [options] <package>...",
        after_help = SEE_ALSO
    )]
    Closure {
        /// Packages to compose, each as <name>@<version>[?arch=<arch>]
        #[arg(value_name = "package", required = true)]
        packages: Vec<PackageId>,
        /// Read packages from <dir> (default: /system/artifacts)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::DEFAULT_ARTIFACT_DIR,
            hide_default_value = true,
        )]
        artifact_dir: PathBuf,
        /// Store closures in <dir> (default: <artifact-dir>/closures)
        #[arg(long, value_name = "dir")]
        closure_dir: Option<PathBuf>,
        /// Copy each file from its blob, rather than hardlink it
        #[arg(long)]
        copy_files: bool,
    },
    /// Lay out a root with the closure of packages as its usr
    #[command(
        override_usage = "ppk compose-env [<profile>...] [options] [-p <package>...] --out <dir>",
        after_help = SEE_ALSO
    )]
    ComposeEnv {
        /// Environment profiles to compose, merged into one
        // Angle brackets in the name, as for help's optional argument below.
        #[arg(value_name = "<profile>", required_unless_present = "packages")]
        profiles: Vec<PathBuf>,
        /// Packages to compose besides the profiles', each as
        /// <name>@<version>[?arch=<arch>]
        #[arg(short = 'p', long = "package", value_name = "package", num_args = 1..)]
        packages: Vec<PackageId>,
        /// Read packages from <dir> (default: /system/artifacts)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::DEFAULT_ARTIFACT_DIR,
            hide_default_value = true,
        )]
        artifact_dir: PathBuf,
        /// Create the root at <dir>, which must not exist
        #[arg(long, value_name = "dir")]
        out: PathBuf,
        /// Create a symlink, given as <path>:<target>
        #[arg(long, value_name = "link")]
        symlink: Vec<ppk::EnvSymlink>,
        /// Create the empty directory <path>
        #[arg(long, value_name = "path")]
        dir: Vec<ppk::EnvDir>,
        /// Hardlink the packages into system/artifacts
        #[arg(long)]
        include_artifacts: bool,
        /// Copy the packages into system/artifacts, rather than hardlink them
        #[arg(long)]
        copy_artifacts: bool,
        /// Copy each file of usr from its blob, rather than hardlink it
        #[arg(long)]
        copy_files: bool,
    },
    /// Run a command, or a shell, with the closure of packages at /usr
    #[command(
        override_usage = "ppk run [<profile>...] [options] [-p <package>...]\n       \
                          [-- <command> [<argument>...]]",
        after_help = SEE_ALSO
    )]
    Run {
        /// Environment profiles whose packages to compose; their symlinks and
        /// directories are for compose-env
        // Angle brackets in the name, as for help's optional argument below.
        #[arg(value_name = "<profile>", required_unless_present = "packages")]
        profiles: Vec<PathBuf>,
        /// Packages to compose besides the profiles', each as
        /// <name>@<version>[?arch=<arch>]
        #[arg(short = 'p', long = "package", value_name = "package", num_args = 1..)]
        packages: Vec<PackageId>,
        /// Read packages from <dir> (default: /system/artifacts)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::DEFAULT_ARTIFACT_DIR,
            hide_default_value = true,
        )]
        artifact_dir: PathBuf,
        /// Use closures in <dir> (default: <artifact-dir>/closures)
        #[arg(long, value_name = "dir")]
        closure_dir: Option<PathBuf>,
        /// Copy each file from its blob, rather than hardlink it
        #[arg(long)]
        copy_files: bool,
        /// Command to run, and its arguments (default: /usr/bin/sh)
        // Angle brackets in the name, as for help's optional argument below.
        #[arg(value_name = "<command>", last = true)]
        command: Vec<OsString>,
    },
    /// Choose packages and write their profile
    #[command(
        override_usage = "ppk resolve [options] [-p <request>...] [-c <request>...]\n       \
                          --from <dir>...",
        after_help = SEE_ALSO
    )]
    Resolve {
        /// Packages to choose, with constraints
        #[arg(
            short = 'p',
            long = "package",
            value_name = "request",
            num_args = 1..,
            required_unless_present = "capabilities"
        )]
        packages: Vec<String>,
        /// Capabilities to provide, with constraints
        #[arg(short = 'c', long = "capability", value_name = "request", num_args = 1..)]
        capabilities: Vec<String>,
        /// Choose from the packages in <dir>
        #[arg(long, value_name = "dir", num_args = 1.., required = true)]
        from: Vec<PathBuf>,
        /// Choose for <arch> (default: ppk's own)
        #[arg(long, value_name = "arch")]
        arch: Option<String>,
        /// Write the profile to <file>
        #[arg(long, value_name = "file")]
        out: Option<PathBuf>,
    },
    /// Print help for ppk or one of its commands
    #[command(override_usage = "ppk help [<command>]", after_help = SEE_ALSO)]
    Help {
        /// Command to print help for
        // clap brackets an optional argument's name without angle brackets,
        // so they are part of the name to give PSC's `[<command>]`.
        #[arg(value_name = "<command>")]
        command: Option<String>,
    },
}

/// What `ppk pack` records in the manifest besides the files. Each pair
/// option takes its two values as separate arguments, so that neither needs
/// quoting or a separator.
#[derive(Args)]
struct MetadataArgs {
    /// Depend on the package <name> at <constraint>
    #[arg(
        long,
        value_names = ["name", "constraint"],
        num_args = 2,
        action = ArgAction::Append,
    )]
    depends: Vec<String>,
    /// Require the capability <name> at <constraint>
    #[arg(
        long,
        value_names = ["name", "constraint"],
        num_args = 2,
        action = ArgAction::Append,
    )]
    requires: Vec<String>,
    /// Provide the capability <name> at <version>
    #[arg(
        long,
        value_names = ["name", "version"],
        num_args = 2,
        action = ArgAction::Append,
    )]
    provides: Vec<String>,
    /// Claim the absolute <path> as a link to <target>
    #[arg(
        long,
        value_names = ["path", "target"],
        num_args = 2,
        action = ArgAction::Append,
    )]
    claims: Vec<String>,
    /// Describe the package in one line
    #[arg(long, value_name = "text", allow_hyphen_values = true)]
    description: Option<String>,
    /// Give the upstream project's URL
    #[arg(long, value_name = "url")]
    homepage: Option<String>,
    /// Give the package's license
    #[arg(long, value_name = "expression", allow_hyphen_values = true)]
    license: Option<String>,
    /// Classify the license's terms (default: unknown)
    #[arg(long, value_name = "class")]
    license_class: Option<String>,
    /// Give the license of the package's source
    #[arg(long, value_name = "expression", allow_hyphen_values = true)]
    source_license: Option<String>,
}

impl MetadataArgs {
    /// The metadata the options give, checked.
    fn metadata(self) -> anyhow::Result<Metadata> {
        let metadata = Metadata {
            description: self.description,
            homepage: self.homepage,
            license: self.license,
            license_class: match self.license_class {
                Some(class) => class.parse()?,
                None => LicenseClass::Unknown,
            },
            source_license: self.source_license,
            dependencies: pairs("--depends", &self.depends, Constraint::from_manifest_string)?,
            requires: pairs(
                "--requires",
                &self.requires,
                Constraint::from_manifest_string,
            )?,
            provides: pairs("--provides", &self.provides, |version| {
                if version.is_empty() {
                    Ok(None)
                } else {
                    Version::parse_relaxed(version).map(Some)
                }
            })?,
            claims: pairs("--claims", &self.claims, |target| Ok(target.to_owned()))?,
        };
        metadata.check()?;
        Ok(metadata)
    }
}

/// The values of the pair option `option`, as a map from each pair's first
/// value to its second, parsed by `parse`. A key given twice is an error.
fn pairs<T>(
    option: &str,
    values: &[String],
    parse: impl Fn(&str) -> anyhow::Result<T>,
) -> anyhow::Result<BTreeMap<String, T>> {
    let mut map = BTreeMap::new();
    // clap gives a pair option's values two at a time.
    for [key, value] in values.as_chunks::<2>().0 {
        let parsed = parse(value).map_err(|err| anyhow::anyhow!("{option} {key}: {err}"))?;
        if map.insert(key.clone(), parsed).is_some() {
            anyhow::bail!("{option} {key}: given more than once");
        }
    }
    Ok(map)
}

fn main() -> ExitCode {
    let program = program_name();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return report_parse_error(&program, &err),
    };
    if let Command::Help { command } = &cli.command {
        return print_help(&program, command.as_deref());
    }
    match run(&program, cli) {
        Ok(status) => status,
        Err(err) => {
            eprintln!("{program}: {}", describe(&err));
            ExitCode::FAILURE
        }
    }
}

/// The environment the profiles at `profiles` describe together, with
/// `packages` added. It must hold a package.
fn environment(profiles: &[PathBuf], packages: Vec<PackageId>) -> anyhow::Result<ppk::Profile> {
    let mut env = ppk::Profile::read_all(profiles)?;
    env.add_packages(packages)?;
    if env.packages.is_empty() {
        anyhow::bail!("the environment holds no packages");
    }
    Ok(env)
}

fn run(program: &str, cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Pack {
            package,
            path,
            artifact_dir,
            metadata,
        } => {
            let metadata = match metadata.metadata() {
                Ok(metadata) => metadata,
                Err(err) => return Ok(report_usage_error(program, &describe(&err))),
            };
            let packed = ppk::pack_with(&package, &metadata, &path, &artifact_dir)?;
            for warning in &packed.warnings {
                eprintln!("{program}: warning: {warning}");
            }
            println!("{}", packed.identity);
        }
        Command::Fetch {
            package,
            artifact_dir,
            from,
        } => {
            let fetched = ppk::fetch(&package, &from, &artifact_dir)?;
            println!("{}", fetched.identity);
        }
        Command::Closure {
            packages,
            artifact_dir,
            closure_dir,
            copy_files,
        } => {
            let closure_dir = closure_dir.unwrap_or_else(|| artifact_dir.join(ppk::CLOSURE_SUBDIR));
            let closed = closure(&packages, &artifact_dir, &closure_dir, copy_files)?;
            println!("{}", closed.identity);
        }
        Command::ComposeEnv {
            profiles,
            packages,
            artifact_dir,
            out,
            symlink,
            dir,
            include_artifacts,
            copy_artifacts,
            copy_files,
        } => {
            let mut env = environment(&profiles, packages)?;
            env.add_extras(symlink, dir)?;
            // Copying is a way of including them, and wins over hardlinking.
            let artifacts = if copy_artifacts {
                ppk::RootArtifacts::Copy
            } else if include_artifacts || env.include_artifacts {
                ppk::RootArtifacts::Include
            } else {
                ppk::RootArtifacts::None
            };
            let extras = ppk::Extras {
                symlinks: env.symlinks,
                dirs: env.dirs,
                artifacts,
                files: if copy_files {
                    ppk::UsrFiles::Copy
                } else {
                    ppk::UsrFiles::Hardlink
                },
            };
            let composed = ppk::compose_env(&env.packages, &artifact_dir, &out, &extras)?;
            println!("{}", composed.identity);
        }
        Command::Run {
            profiles,
            packages,
            artifact_dir,
            closure_dir,
            copy_files,
            command,
        } => {
            // A run keeps the caller's root, so it takes only the packages.
            let env = environment(&profiles, packages)?;
            let closure_dir = closure_dir.unwrap_or_else(|| artifact_dir.join(ppk::CLOSURE_SUBDIR));
            let closed = closure(&env.packages, &artifact_dir, &closure_dir, copy_files)?;
            ppk::environment::enter(&closed.tree_path)?;
            let command = if command.is_empty() {
                vec![OsString::from(ppk::environment::SHELL)]
            } else {
                command
            };
            return Ok(exec(program, &command));
        }
        Command::Resolve {
            packages,
            capabilities,
            from,
            arch,
            out,
        } => {
            let requests = match requests(&packages, &capabilities) {
                Ok(requests) => requests,
                Err(err) => return Ok(report_usage_error(program, &describe(&err))),
            };
            let arch = arch.as_deref().unwrap_or(ppk::HOST_ARCH);
            let profile = ppk::resolve(&requests, &from, arch)?.profile().render()?;
            match out {
                Some(out) => {
                    std::fs::write(&out, profile).map_err(|err| {
                        anyhow::Error::new(err).context(out.display().to_string())
                    })?;
                }
                None => print!("{profile}"),
            }
        }
        Command::Help { .. } => unreachable!("help is handled before run"),
    }
    Ok(ExitCode::SUCCESS)
}

/// The requests `-p` and `-c` give, packages first.
fn requests(packages: &[String], capabilities: &[String]) -> anyhow::Result<Vec<ppk::Request>> {
    let packages = packages
        .iter()
        .map(|s| ppk::Request::parse_package(s).map_err(|err| err.context("-p")));
    let capabilities = capabilities
        .iter()
        .map(|s| ppk::Request::parse_capability(s).map_err(|err| err.context("-c")));
    packages.chain(capabilities).collect()
}

/// The closure of `packages`, copying its files if `copy_files`.
fn closure(
    packages: &[PackageId],
    artifact_dir: &Path,
    closure_dir: &Path,
    copy_files: bool,
) -> anyhow::Result<ppk::Closed> {
    if copy_files {
        ppk::closure_copying(packages, artifact_dir, closure_dir)
    } else {
        ppk::closure(packages, artifact_dir, closure_dir)
    }
}

/// Replace ppk with `command`, which then decides the exit status. Returns
/// only if the command could not be started, with the status shells use.
fn exec(program: &str, command: &[OsString]) -> ExitCode {
    let (name, args) = command
        .split_first()
        .expect("run supplies the shell when no command is given");
    let err = std::process::Command::new(name).args(args).exec();
    let status = match err.kind() {
        io::ErrorKind::NotFound => EXIT_NOT_FOUND,
        io::ErrorKind::PermissionDenied => EXIT_CANNOT_EXECUTE,
        _ => EXIT_FAILURE,
    };
    let err = anyhow::Error::new(err).context(name.to_string_lossy().into_owned());
    eprintln!("{program}: {}", describe(&err));
    ExitCode::from(status)
}

/// `ppk help [<command>]`: the same text as `--help` for ppk or that command.
fn print_help(program: &str, command: Option<&str>) -> ExitCode {
    let mut cli = Cli::command();
    cli.build();
    let target = match command {
        None => &mut cli,
        Some(name) => match cli.find_subcommand_mut(name) {
            Some(subcommand) => subcommand,
            None => return report_usage_error(program, &format!("unknown command '{name}'")),
        },
    };
    match target.print_help() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
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
    report_usage_error(program, &usage_message(err))
}

fn report_usage_error(program: &str, message: &str) -> ExitCode {
    eprintln!("{program}: {message}");
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
        (ErrorKind::InvalidSubcommand, _) => match err.get(ContextKind::InvalidSubcommand) {
            Some(ContextValue::String(command)) => format!("unknown command '{command}'"),
            _ => clap_first_line(err),
        },
        // Required only without -c.
        (ErrorKind::MissingRequiredArgument, ["--package <request>..."]) => {
            "missing -p <request>... or -c <request>...".to_owned()
        }
        (ErrorKind::MissingRequiredArgument, [arg]) if arg.starts_with('-') => {
            format!("missing option {arg}")
        }
        // Required only without -p, and named with its brackets already.
        (ErrorKind::MissingRequiredArgument, ["<<profile>>..."]) => {
            "missing <profile>... or -p <package>...".to_owned()
        }
        (ErrorKind::MissingRequiredArgument, [_]) => format!("missing argument {}", args[0]),
        (ErrorKind::MissingRequiredArgument, [_, ..]) => {
            format!("missing arguments {}", args.join(" "))
        }
        (ErrorKind::ValueValidation, _) if err.source().is_some() => {
            err.source().map(ToString::to_string).unwrap_or_default()
        }
        _ => clap_first_line(err),
    }
}

/// The first line of clap's own message, without its `error: ` prefix.
fn clap_first_line(err: &clap::Error) -> String {
    let rendered = err.render().to_string();
    let first = rendered.lines().next().unwrap_or_default();
    first.strip_prefix("error: ").unwrap_or(first).to_owned()
}

/// The error and its causes on one line, outermost first, without the
/// `(os error N)` that Rust appends to system errors. Some errors carry
/// more after it, such as tempfile's ` at path ...`, so it is cut out
/// wherever it is.
fn describe(err: &anyhow::Error) -> String {
    let causes: Vec<String> = err
        .chain()
        .map(|cause| {
            let mut text = cause.to_string();
            if let Some(start) = text.find(" (os error ")
                && let Some(len) = text[start..].find(')')
            {
                text.replace_range(start..=start + len, "");
            }
            text
        })
        .collect();
    causes.join(": ")
}
