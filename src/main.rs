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
    /// List the packages in the current closure or the store
    #[command(override_usage = "ppk list [options]", after_help = SEE_ALSO)]
    List {
        /// List every stored package, with `*` beside those in the current closure
        #[arg(long)]
        all: bool,
        /// Read <dir>/usr/inventory (default: /)
        #[arg(
            long,
            value_name = "dir",
            default_value = "/",
            hide_default_value = true
        )]
        root: PathBuf,
        /// Read packages from <dir> (default: /system/artifacts)
        #[arg(long, value_name = "dir", default_value = ppk::DEFAULT_ARTIFACT_DIR, hide_default_value = true)]
        artifact_dir: PathBuf,
    },
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
    /// Fetch a package and what it needs from another artifact directory
    #[command(
        override_usage = "ppk fetch [options] --from <dir> <request>",
        after_help = SEE_ALSO
    )]
    Fetch {
        /// Package to fetch, as a name, constraint or exact package ID
        #[arg(value_name = "request")]
        package: String,
        /// Store packages in <dir> (default: /system/artifacts)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::DEFAULT_ARTIFACT_DIR,
            hide_default_value = true,
        )]
        artifact_dir: PathBuf,
        /// Copy from the artifact directory <dir>, rather than from the
        /// configured repositories
        #[arg(long, value_name = "dir")]
        from: Option<PathBuf>,
        /// Read the repositories from <dir> (default: /conf/ppk)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::config::DEFAULT_CONFIG_DIR,
            hide_default_value = true
        )]
        config_dir: PathBuf,
        /// Keep what is seen of each repository in <dir> (default: /var/state/ppk)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::config::DEFAULT_STATE_DIR,
            hide_default_value = true
        )]
        state_dir: PathBuf,
        /// Copy exactly the package ID given, without resolving what it needs
        #[arg(long)]
        no_deps: bool,
        /// Choose for <arch> (default: ppk's own)
        #[arg(long, value_name = "arch", conflicts_with = "no_deps")]
        arch: Option<String>,
        /// Replace an existing package (requires --no-deps; for bootstraps)
        #[arg(long, requires = "no_deps")]
        replace: bool,
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
    /// Publish a repository: its catalog, indexes and channels
    #[command(
        override_usage = "ppk repo <command> [options]",
        subcommand_value_name = "command",
        after_help = SEE_ALSO
    )]
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Name a closure, or show what names name
    #[command(
        override_usage = "ppk name [options] [<name> [<closure>]]\n       ppk name [options] --remove <name>",
        after_help = SEE_ALSO
    )]
    Name {
        /// The name, such as SYSTEM or INITRAMFS
        #[arg(value_name = "name")]
        name: Option<String>,
        /// The closure to name, as its identity: sha256:<hex>
        #[arg(value_name = "closure", conflicts_with = "remove")]
        closure: Option<ppk::Identity>,
        /// Remove the name, leaving its closure
        #[arg(long, requires = "name")]
        remove: bool,
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
        override_usage = "ppk run [<profile>...] [options] [-p <request>...]\n       \
                          [-- <command> [<argument>...]]",
        after_help = SEE_ALSO
    )]
    Run {
        /// Environment profiles whose packages to compose; their symlinks and
        /// directories are for compose-env
        // Angle brackets in the name, as for help's optional argument below.
        #[arg(value_name = "<profile>", required_unless_present = "packages")]
        profiles: Vec<PathBuf>,
        /// Packages to resolve besides the profiles', as names, constraints
        /// or exact package IDs
        #[arg(short = 'p', long = "package", value_name = "request", num_args = 1..)]
        packages: Vec<String>,
        /// Compose exactly the package IDs given, without resolving their needs
        #[arg(long)]
        no_deps: bool,
        /// Choose for <arch> (default: ppk's own)
        #[arg(long, value_name = "arch", conflicts_with = "no_deps")]
        arch: Option<String>,
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
    /// Give the package's file or directory <path> the security descriptor
    /// <descriptor>, in hexadecimal
    #[arg(
        long,
        value_names = ["path", "descriptor"],
        num_args = 2,
        action = ArgAction::Append,
    )]
    descriptor: Vec<String>,
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
    /// What the options give `pack`, checked.
    fn options(mut self) -> anyhow::Result<ppk::PackOptions> {
        let descriptors = pairs(
            "--descriptor",
            &std::mem::take(&mut self.descriptor),
            |sd| sd.parse::<ppk::SecurityDescriptor>(),
        )?;
        Ok(ppk::PackOptions {
            metadata: self.metadata()?,
            descriptors,
            replace: false,
        })
    }

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

/// The options every command that publishes a repository takes.
#[derive(Args)]
struct Publishing {
    /// The repository, an artifact directory (default: /system/artifacts)
    #[arg(
        long,
        value_name = "dir",
        default_value = ppk::DEFAULT_ARTIFACT_DIR,
        hide_default_value = true,
    )]
    artifact_dir: PathBuf,
    /// Sign with the ML-DSA-65 private key in the PEM file <file>
    /// (default: $PPK_REPO_KEY)
    #[arg(long, value_name = "file")]
    key: Option<PathBuf>,
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Publish the catalog and leading from what the repository holds
    #[command(override_usage = "ppk repo update [options]", after_help = SEE_ALSO)]
    Update {
        #[command(flatten)]
        publishing: Publishing,
        /// Refuse to publish if leading would leave a package out
        #[arg(long)]
        strict: bool,
    },
    /// Make a new index a snapshot of an index or channel
    #[command(
        override_usage = "ppk repo snapshot [options] <name> [--from <index>]",
        after_help = SEE_ALSO
    )]
    Snapshot {
        /// The new index's name, such as 2026.10.0
        #[arg(value_name = "name")]
        name: String,
        /// The index or channel to take (default: leading)
        #[arg(long, value_name = "index", default_value = ppk::repo::LEADING)]
        from: String,
        #[command(flatten)]
        publishing: Publishing,
    },
    /// Make a channel follow an index or channel
    #[command(
        override_usage = "ppk repo channel [options] <name> <target>",
        after_help = SEE_ALSO
    )]
    Channel {
        /// The channel, such as stable
        #[arg(value_name = "name")]
        name: String,
        /// The index or channel it follows
        #[arg(value_name = "target")]
        target: String,
        /// Mark the channel as one that won't move again
        #[arg(long)]
        ended: bool,
        #[command(flatten)]
        publishing: Publishing,
    },
    /// Take a version back from new resolutions, or with --undo, restore it
    #[command(override_usage = "ppk repo yank [options] <package>", after_help = SEE_ALSO)]
    Yank {
        /// The version, as <name>@<version>[?arch=<arch>]
        #[arg(value_name = "package")]
        package: PackageId,
        /// Restore a yanked version
        #[arg(long)]
        undo: bool,
        #[command(flatten)]
        publishing: Publishing,
    },
    /// Delete a version, its manifest and files, leaving a tombstone
    #[command(
        override_usage = "ppk repo delete [options] <package> --reason <text>",
        after_help = SEE_ALSO
    )]
    Delete {
        /// The version, as <name>@<version>[?arch=<arch>]
        #[arg(value_name = "package")]
        package: PackageId,
        /// Why, which the tombstone records
        #[arg(long, value_name = "text")]
        reason: String,
        #[command(flatten)]
        publishing: Publishing,
    },
    /// Print a repository's pointer file, checking its signature with a key
    #[command(override_usage = "ppk repo show [options]", after_help = SEE_ALSO)]
    Show {
        /// The repository (default: /system/artifacts)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::DEFAULT_ARTIFACT_DIR,
            hide_default_value = true,
        )]
        artifact_dir: PathBuf,
        /// Check the signature against the public key <key>
        #[arg(long, value_name = "key")]
        public_key: Option<String>,
    },
    /// Add a repository for this machine to fetch from
    #[command(
        override_usage = "ppk repo add [options] <name> <path>",
        after_help = SEE_ALSO
    )]
    Add {
        /// Its name, such as peios
        #[arg(value_name = "name")]
        name: String,
        /// Where it is
        #[arg(value_name = "path")]
        path: PathBuf,
        /// Check its pointer file against the public key <key>
        #[arg(long, value_name = "key")]
        public_key: Option<String>,
        /// Follow the index or channel <index> for bare names, such as stable
        #[arg(long, value_name = "index")]
        track: Option<String>,
        /// Write it in <dir> (default: /lcl/conf/ppk)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::config::DEFAULT_LOCAL_CONFIG_DIR,
            hide_default_value = true
        )]
        config_dir: PathBuf,
    },
    /// Stop fetching from a repository
    #[command(override_usage = "ppk repo remove [options] <name>", after_help = SEE_ALSO)]
    Remove {
        /// Its name
        #[arg(value_name = "name")]
        name: String,
        /// Remove it from <dir> (default: /lcl/conf/ppk)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::config::DEFAULT_LOCAL_CONFIG_DIR,
            hide_default_value = true
        )]
        config_dir: PathBuf,
    },
    /// List the repositories this machine fetches from, checking each
    #[command(override_usage = "ppk repo list [options]", after_help = SEE_ALSO)]
    List {
        /// Read the repositories from <dir> (default: /conf/ppk)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::config::DEFAULT_CONFIG_DIR,
            hide_default_value = true
        )]
        config_dir: PathBuf,
        /// Keep what is seen of each in <dir> (default: /var/state/ppk)
        #[arg(
            long,
            value_name = "dir",
            default_value = ppk::config::DEFAULT_STATE_DIR,
            hide_default_value = true
        )]
        state_dir: PathBuf,
    },
    /// Print the public key of a private key, for a repository's configuration
    #[command(override_usage = "ppk repo public-key --key <file>", after_help = SEE_ALSO)]
    PublicKey {
        /// The ML-DSA-65 private key in the PEM file <file> (default: $PPK_REPO_KEY)
        #[arg(long, value_name = "file")]
        key: Option<PathBuf>,
    },
}

/// The private key at `path`, or at `$PPK_REPO_KEY` if no path is given.
fn load_key(path: Option<&Path>) -> anyhow::Result<ppk::repo::PrivateKey> {
    let from_env = std::env::var_os("PPK_REPO_KEY").map(PathBuf::from);
    let Some(path) = path.map(Path::to_path_buf).or(from_env) else {
        anyhow::bail!("no key: give --key <file>, or set PPK_REPO_KEY");
    };
    ppk::repo::PrivateKey::load(&path)
}

/// Run the repository command `command`.
fn repo(program: &str, command: RepoCommand) -> anyhow::Result<()> {
    let key = |publishing: &Publishing| load_key(publishing.key.as_deref());
    let report = |published: ppk::repo::Published| {
        for left in &published.left_out {
            eprintln!(
                "{program}: warning: {} left out of leading: {}",
                left.id, left.reason
            );
        }
        print!("{}", published.pointer.render());
    };
    match command {
        RepoCommand::Update { publishing, strict } => {
            report(ppk::repo::update(
                &publishing.artifact_dir,
                &key(&publishing)?,
                strict,
            )?);
        }
        RepoCommand::Snapshot {
            name,
            from,
            publishing,
        } => {
            let pointer =
                ppk::repo::snapshot(&publishing.artifact_dir, &key(&publishing)?, &name, &from)?;
            print!("{}", pointer.render());
        }
        RepoCommand::Channel {
            name,
            target,
            ended,
            publishing,
        } => {
            let pointer = ppk::repo::channel(
                &publishing.artifact_dir,
                &key(&publishing)?,
                &name,
                &target,
                ended,
            )?;
            print!("{}", pointer.render());
        }
        RepoCommand::Yank {
            package,
            undo,
            publishing,
        } => {
            report(ppk::repo::yank(
                &publishing.artifact_dir,
                &key(&publishing)?,
                &package,
                undo,
            )?);
        }
        RepoCommand::Delete {
            package,
            reason,
            publishing,
        } => {
            report(ppk::repo::delete(
                &publishing.artifact_dir,
                &key(&publishing)?,
                &package,
                &reason,
            )?);
        }
        RepoCommand::Show {
            artifact_dir,
            public_key,
        } => {
            let public_key: Option<ppk::repo::PublicKey> =
                public_key.as_deref().map(str::parse).transpose()?;
            match ppk::repo::read_pointer(&artifact_dir, public_key.as_ref())? {
                Some(pointer) => print!("{}", pointer.render()),
                None => anyhow::bail!("{}: not published as a repository", artifact_dir.display()),
            }
        }
        RepoCommand::Add {
            name,
            path,
            public_key,
            track,
            config_dir,
        } => {
            let key = public_key.as_deref().map(str::parse).transpose()?;
            if let Some(track) = &track {
                ppk::repo::check_ref_name(track)?;
            }
            ppk::config::add_repository(
                &config_dir,
                &ppk::config::Repository {
                    name,
                    path,
                    key,
                    track,
                },
            )?;
        }
        RepoCommand::Remove { name, config_dir } => {
            ppk::config::remove_repository(&config_dir, &name)?;
        }
        RepoCommand::List {
            config_dir,
            state_dir,
        } => {
            for repository in ppk::config::repositories(&config_dir)? {
                let published = match repository.pointer(&state_dir)? {
                    Some(pointer) => format!(
                        "sequence {}, signed {}{}",
                        pointer.sequence,
                        pointer.signed,
                        match repository.key {
                            Some(_) => ", signature checked",
                            None => "",
                        }
                    ),
                    None => "not published".to_owned(),
                };
                let track = repository.track.as_deref().unwrap_or("-");
                println!(
                    "{} {} #{track} ({published})",
                    repository.name,
                    repository.path.display()
                );
            }
        }
        RepoCommand::PublicKey { key } => {
            println!("{}", load_key(key.as_deref())?.public());
        }
    }
    Ok(())
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
            let options = match metadata.options() {
                Ok(options) => options,
                Err(err) => return Ok(report_usage_error(program, &describe(&err))),
            };
            let packed = ppk::pack_with_options(&package, &options, &path, &artifact_dir)?;
            for warning in &packed.warnings {
                eprintln!("{program}: warning: {warning}");
            }
            warn_unstamped(program, packed.unstamped);
            println!("{}", packed.identity);
        }
        Command::Fetch {
            package,
            artifact_dir,
            from,
            config_dir,
            state_dir,
            replace,
            no_deps,
            arch,
        } => {
            let Some(from) = from else {
                if no_deps {
                    return Ok(report_usage_error(program, "--no-deps needs --from"));
                }
                let inputs = match resolution_inputs(&[package], Vec::new(), arch.as_deref()) {
                    Ok(inputs) => inputs,
                    Err(err) => return Ok(report_usage_error(program, &describe(&err))),
                };
                let mut requests = ppk::exact_requests(&inputs.pins)?;
                requests.extend(inputs.requests);
                let repositories = ppk::config::repositories(&config_dir)?;
                let settled =
                    ppk::config::resolve(&requests, &repositories, &state_dir, &inputs.arch)?;
                for package in settled.resolution.packages {
                    if ppk::packed(&package.id, &artifact_dir)? == Some(package.identity) {
                        continue;
                    }
                    let fetched = ppk::fetch(&package.id, &package.artifact_dir, &artifact_dir)?;
                    warn_unstamped(program, fetched.unstamped);
                    println!("{}", fetched.identity);
                }
                return Ok(ExitCode::SUCCESS);
            };
            if no_deps {
                let id = match package.parse() {
                    Ok(id) => id,
                    Err(err) => return Ok(report_usage_error(program, &describe(&err))),
                };
                let fetched = if replace {
                    ppk::fetch_replacing(&id, &from, &artifact_dir)?
                } else {
                    ppk::fetch(&id, &from, &artifact_dir)?
                };
                warn_unstamped(program, fetched.unstamped);
                println!("{}", fetched.identity);
            } else {
                let inputs = match resolution_inputs(&[package], Vec::new(), arch.as_deref()) {
                    Ok(inputs) => inputs,
                    Err(err) => return Ok(report_usage_error(program, &describe(&err))),
                };
                let resolved = ppk::resolve_pinned(
                    &inputs.requests,
                    &inputs.pins,
                    std::slice::from_ref(&from),
                    &inputs.arch,
                )?;
                for package in resolved.packages {
                    if ppk::packed(&package.id, &artifact_dir)? == Some(package.identity) {
                        continue;
                    }
                    let fetched = ppk::fetch(&package.id, &from, &artifact_dir)?;
                    warn_unstamped(program, fetched.unstamped);
                    println!("{}", fetched.identity);
                }
            }
        }
        Command::List {
            all,
            root,
            artifact_dir,
        } => {
            let usr = root.join("usr");
            let inventory = ppk::read_inventory(&usr)?;
            if all {
                for (id, identity) in ppk::stored_packages(&artifact_dir)? {
                    let member = inventory.as_ref().is_some_and(|inventory| {
                        inventory
                            .packages
                            .iter()
                            .any(|package| package.identity == identity && package.id == id)
                    });
                    println!("{} {id}", if member { '*' } else { ' ' });
                }
            } else {
                let inventory = inventory.ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}: this /usr wasn't composed by a ppk that records an inventory",
                        usr.display()
                    )
                })?;
                let mut ids: Vec<_> = inventory
                    .packages
                    .iter()
                    .map(|package| package.id.to_string())
                    .collect();
                ids.sort();
                for id in ids {
                    println!("{id}");
                }
            }
        }
        Command::Closure {
            packages,
            artifact_dir,
            closure_dir,
            copy_files,
        } => {
            let closure_dir = closure_dir.unwrap_or_else(|| artifact_dir.join(ppk::CLOSURE_SUBDIR));
            let closed = closure(&packages, &artifact_dir, &closure_dir, copy_files)?;
            warn_unstamped(program, closed.unstamped);
            println!("{}", closed.identity);
        }
        Command::Repo { command } => repo(program, command)?,
        Command::Name {
            name,
            closure,
            remove,
            artifact_dir,
            closure_dir,
        } => {
            let closure_dir = closure_dir.unwrap_or_else(|| artifact_dir.join(ppk::CLOSURE_SUBDIR));
            match (name, closure) {
                (Some(name), _) if remove => ppk::remove_closure_name(&closure_dir, &name)?,
                (Some(name), Some(closure)) => ppk::name_closure(&closure_dir, &name, &closure)?,
                (Some(name), None) => match ppk::named_closure(&closure_dir, &name)? {
                    Some(identity) => println!("{identity}"),
                    None => anyhow::bail!("{name}: names no closure in {}", closure_dir.display()),
                },
                (None, _) => {
                    for (name, identity) in ppk::closure_names(&closure_dir)? {
                        println!("{name} {identity}");
                    }
                }
            }
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
            // The profile asks; resolving answers, with what that needs.
            let resolved = ppk::resolve(
                &env.packages,
                std::slice::from_ref(&artifact_dir),
                env.arch.as_deref().unwrap_or(ppk::HOST_ARCH),
            )?;
            let composed =
                ppk::compose_env_for(&resolved.ids(), &env.packages, &artifact_dir, &out, &extras)?;
            warn_unstamped(program, composed.unstamped);
            println!("{}", composed.identity);
        }
        Command::Run {
            profiles,
            packages,
            artifact_dir,
            closure_dir,
            copy_files,
            command,
            no_deps,
            arch,
        } => {
            // A run keeps the caller's root, so it takes only the packages.
            // What was asked for is what the closure's inventory records: an
            // exact ID asks for exactly that package.
            let (packages, requests) = if no_deps {
                let packages = match packages.iter().map(|s| s.parse()).collect() {
                    Ok(packages) => packages,
                    Err(err) => return Ok(report_usage_error(program, &describe(&err))),
                };
                let env = environment(&profiles, packages)?;
                (env.exact_packages()?, env.packages)
            } else if packages.is_empty() {
                let env = environment(&profiles, Vec::new())?;
                let resolved = ppk::resolve(
                    &env.packages,
                    std::slice::from_ref(&artifact_dir),
                    env.arch(),
                )?;
                (resolved.ids(), env.packages)
            } else {
                let profile = ppk::Profile::read_all(&profiles)?;
                let arch = arch.as_deref().or(profile.arch.as_deref());
                let inputs = match resolution_inputs(&packages, Vec::new(), arch) {
                    Ok(inputs) => inputs,
                    Err(err) => return Ok(report_usage_error(program, &describe(&err))),
                };
                let mut requests = profile.packages;
                requests.extend(inputs.requests);
                let resolved = ppk::resolve_pinned(
                    &requests,
                    &inputs.pins,
                    std::slice::from_ref(&artifact_dir),
                    &inputs.arch,
                )?;
                let mut asked = ppk::exact_requests(&inputs.pins)?;
                asked.extend(requests);
                (resolved.ids(), asked)
            };
            let closure_dir = closure_dir.unwrap_or_else(|| artifact_dir.join(ppk::CLOSURE_SUBDIR));
            let files = match copy_files {
                true => ppk::UsrFiles::Copy,
                false => ppk::UsrFiles::Hardlink,
            };
            let closed =
                ppk::closure_for(&packages, &requests, &artifact_dir, &closure_dir, files)?;
            warn_unstamped(program, closed.unstamped);
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
            let profile = ppk::resolve(&requests, &from, arch)?.profile()?.render()?;
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

/// Report the `unstamped` files and directories that lack security metadata
/// their manifests record, as PSPU 5.16 asks of a consumer that can't set it.
fn warn_unstamped(program: &str, unstamped: usize) {
    let (what, record) = match unstamped {
        0 => return,
        1 => (
            "1 file or directory lacks".to_owned(),
            "its manifest records",
        ),
        n => (
            format!("{n} files and directories lack"),
            "their manifests record",
        ),
    };
    eprintln!(
        "{program}: warning: {what} security metadata this system would not let ppk give; \
         {record} it"
    );
}

/// Requests and exact pins for one resolution. A full ID can select a foreign
/// architecture; unqualified requests use `--arch`, or ppk's own architecture.
struct ResolutionInputs {
    requests: Vec<ppk::Request>,
    pins: Vec<PackageId>,
    arch: String,
}

fn resolution_inputs(
    packages: &[String],
    mut pins: Vec<PackageId>,
    arch: Option<&str>,
) -> anyhow::Result<ResolutionInputs> {
    let default_arch = arch.unwrap_or(ppk::HOST_ARCH);
    let mut requests = Vec::new();
    let mut unqualified = false;
    for package in packages {
        if package.contains('@') {
            let mut id: PackageId = package.parse()?;
            if !package.contains('?') {
                id = PackageId::new(id.name(), id.version(), default_arch)?;
                unqualified = true;
            }
            pins.push(id);
        } else {
            requests.push(ppk::Request::parse_package(package)?);
            unqualified = true;
        }
    }
    let arch = if unqualified || arch.is_some() {
        default_arch
    } else {
        pins.first().map_or(default_arch, PackageId::arch)
    };
    for id in &pins {
        if id.arch() != arch {
            anyhow::bail!("{id}: resolving packages for {arch}");
        }
    }
    Ok(ResolutionInputs {
        arch: arch.to_owned(),
        requests,
        pins,
    })
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
            let value = if err.render().to_string().contains("ppk run ") {
                "request"
            } else {
                "package"
            };
            format!("missing <profile>... or -p <{value}>...")
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
