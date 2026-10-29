use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "envira", version, about = "Install, update and track your Linux toolset", long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Args)]
pub struct Global {
    /// Catalog file or directory to use instead of the embedded catalog (repeatable).
    #[arg(long, global = true, env = "ENVIRA_CATALOG", value_delimiter = ':')]
    pub catalog: Vec<PathBuf>,
    /// Ignore ~/.config/envira/catalog.d.
    #[arg(long, global = true)]
    pub no_overlay: bool,
    /// Where `any`-scope methods install: system (needs sudo) or user. Default: system when sudo is available.
    #[arg(long, global = true, value_enum)]
    pub scope: Option<ScopeArg>,
    /// Stop and fail a step that runs longer than this many seconds.
    #[arg(
        long,
        global = true,
        env = "ENVIRA_STEP_TIMEOUT",
        value_name = "SECONDS"
    )]
    pub step_timeout: Option<u64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ScopeArg {
    User,
    System,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the status of every catalog item.
    Status {
        #[arg(long)]
        json: bool,
        /// Check for newer versions (cached for 6 hours; uses the network).
        #[arg(long)]
        latest: bool,
        /// With --latest: ignore the cache.
        #[arg(long)]
        refresh: bool,
    },
    /// Install whatever the selected bundles need.
    Sync {
        /// Add a bundle to the bundles this machine keeps in sync (repeatable).
        #[arg(long = "bundle", short = 'b')]
        bundles: Vec<String>,
        #[command(flatten)]
        run: RunOpts,
    },
    /// Install tools or apply configs (ids or bundle names), with their dependencies.
    Install {
        #[arg(required = true)]
        items: Vec<String>,
        /// Reinstall even when already present.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        run: RunOpts,
    },
    /// Update outdated tools (all, or the given ids/bundles) in place.
    Update {
        items: Vec<String>,
        /// Only list what is outdated.
        #[arg(long)]
        check: bool,
        /// Ignore cached latest versions.
        #[arg(long, conflicts_with = "offline")]
        refresh: bool,
        /// Use cached latest versions only (no network).
        #[arg(long)]
        offline: bool,
        #[command(flatten)]
        run: RunOpts,
    },
    /// Record tools installed outside envira (adoptable) as managed, without reinstalling.
    Adopt {
        /// Tool ids or bundles; default: every adoptable tool.
        items: Vec<String>,
        /// Show what would be adopted.
        #[arg(long)]
        dry_run: bool,
    },
    /// Retry what failed or was blocked in the last run.
    Retry {
        #[command(flatten)]
        run: RunOpts,
    },
    /// Apply or revert configs.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Show what `sync` (or `install <items>`) would do.
    Plan {
        items: Vec<String>,
        /// Print the plan as a shell script.
        #[arg(long)]
        script: bool,
    },
    /// Regenerate the managed shell environment (~/.config/envira) and rc blocks.
    Env,
    /// Show the log of the last run (or of one item in it).
    Log {
        item: Option<String>,
        #[arg(long)]
        run: Option<String>,
    },
    /// Why an item has its status: detect/check commands with their exit codes and output,
    /// the install route, recorded state and what installing it would run.
    Explain { item: String },
    /// Machine facts, catalog layers, PATH and shell integration.
    Doctor,
    /// Catalog utilities.
    Catalog {
        #[command(subcommand)]
        cmd: CatalogCmd,
    },
    /// Idempotent file edits used by config actions.
    #[command(hide = true)]
    Fs {
        #[command(subcommand)]
        cmd: FsCmd,
    },
    /// Download a GitHub release asset and install binaries from it.
    #[command(hide = true)]
    FetchRelease {
        #[arg(long)]
        repo: String,
        /// Asset name globs, tried in order.
        #[arg(long = "asset", num_args = 1.., required = true)]
        assets: Vec<String>,
        #[arg(long = "bin", num_args = 1.., required = true)]
        bins: Vec<String>,
        #[arg(long)]
        to: PathBuf,
        #[arg(long)]
        tag: Option<String>,
    },
}

#[derive(Debug, Clone, Args)]
pub struct RunOpts {
    /// Do not ask for confirmation.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Print the plan and exit.
    #[arg(long)]
    pub dry_run: bool,
    /// Do not print step output.
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    Apply {
        #[arg(required = true)]
        items: Vec<String>,
        #[command(flatten)]
        run: RunOpts,
    },
    Revert {
        #[arg(required = true)]
        items: Vec<String>,
        #[command(flatten)]
        run: RunOpts,
    },
}

#[derive(Debug, Subcommand)]
pub enum CatalogCmd {
    /// Validate the merged catalog (exit 1 on errors).
    Check,
    /// Print the JSON Schema of catalog files.
    Schema,
    /// Print the merged catalog as TOML.
    Show,
}

#[derive(Debug, Subcommand)]
pub enum FsCmd {
    /// Whole file with fixed content.
    Write {
        path: String,
        #[arg(long)]
        content: String,
        /// Octal mode, e.g. 644.
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        check: bool,
    },
    /// Marked block inside a file.
    Block {
        path: String,
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "#")]
        comment: String,
        #[arg(long, required_unless_present = "remove")]
        content: Option<String>,
        #[arg(long)]
        remove: bool,
        #[arg(long)]
        check: bool,
    },
    /// Set a dotted key in a TOML file.
    TomlSet {
        file: String,
        key: String,
        value: String,
        #[arg(long)]
        check: bool,
    },
    /// Set a key in an INI section.
    IniSet {
        file: String,
        section: String,
        key: String,
        value: String,
        #[arg(long)]
        check: bool,
    },
    /// Set one whole line, found by a regex (inserted after `--after` or appended when absent).
    Line {
        file: String,
        #[arg(long = "match")]
        pattern: String,
        #[arg(long)]
        line: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        check: bool,
    },
    /// Symbolic link.
    Link {
        target: String,
        link: String,
        #[arg(long)]
        check: bool,
    },
    /// Undo envira's changes to a file (or one key).
    Restore {
        path: String,
        #[arg(long)]
        key: Option<String>,
    },
}
