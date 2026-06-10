use clap::{Args, Parser, Subcommand};

use crate::{
    engine::{CommandName, CommandRequest, InstallMode, InterfaceMode, OutputFormat},
    planner::PlannerRequest,
    verifier::VerificationProfile,
};

#[derive(Debug, Parser)]
#[command(
    name = "envira",
    version,
    about = "Catalog-driven software environment management tool",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    pub fn into_request(self) -> CommandRequest {
        self.command.into_request()
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    #[command(about = "Inspect the loaded catalog and its default bundles")]
    Catalog(OutputArgs),
    #[command(
        about = "Preview catalog actions without executing commands",
        after_help = "When no selection flag is provided, envira selects the catalog's default_bundles."
    )]
    Plan(WorkflowArgs),
    #[command(
        about = "Execute catalog commands for selected items",
        after_help = "When no selection flag is provided, envira selects the catalog's default_bundles."
    )]
    Install(InstallArgs),
    #[command(
        about = "Run catalog verification for selected items",
        after_help = "When no selection flag is provided, envira selects the catalog's default_bundles."
    )]
    Verify(VerifyArgs),
    #[command(about = "Open the interactive terminal interface")]
    Tui,
}

impl Command {
    pub fn into_request(self) -> CommandRequest {
        match self {
            Self::Catalog(args) => args.into_request(CommandName::Catalog),
            Self::Plan(args) => args.into_request(CommandName::Plan),
            Self::Install(args) => args.into_request(CommandName::Install),
            Self::Verify(args) => args.into_request(CommandName::Verify),
            Self::Tui => {
                CommandRequest::new(CommandName::Tui, InterfaceMode::Tui, OutputFormat::Text)
            }
        }
    }
}

impl OutputArgs {
    fn into_request(self, command: CommandName) -> CommandRequest {
        CommandRequest::new(command, InterfaceMode::Headless, self.format)
    }
}

#[derive(Debug, Clone, Args)]
pub struct OutputArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render headless output as text or json"
    )]
    format: OutputFormat,
}

#[derive(Debug, Clone, Args)]
pub struct SelectionArgs {
    #[arg(
        long,
        value_name = "ITEM",
        conflicts_with_all = ["bundle", "all"],
        help = "Select one catalog item"
    )]
    item: Option<String>,
    #[arg(
        long,
        value_name = "BUNDLE",
        conflicts_with_all = ["item", "all"],
        help = "Select one catalog bundle"
    )]
    bundle: Option<String>,
    #[arg(
        long,
        conflicts_with_all = ["item", "bundle"],
        help = "Select every catalog item"
    )]
    all: bool,
}

impl SelectionArgs {
    fn into_planner_request(self) -> Option<PlannerRequest> {
        if self.all {
            Some(PlannerRequest::all_items())
        } else if let Some(item) = self.item {
            Some(PlannerRequest::item(item))
        } else {
            self.bundle.map(PlannerRequest::bundle)
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct WorkflowArgs {
    #[command(flatten)]
    output: OutputArgs,
    #[command(flatten)]
    selection: SelectionArgs,
}

impl WorkflowArgs {
    fn into_request(self, command: CommandName) -> CommandRequest {
        let mut request = self.output.into_request(command);

        if let Some(planner_request) = self.selection.into_planner_request() {
            request = request.with_planner_request(planner_request);
        }

        request
    }
}

#[derive(Debug, Clone, Args)]
pub struct VerifyArgs {
    #[command(flatten)]
    output: OutputArgs,
    #[command(flatten)]
    selection: VerifySelectionArgs,
    #[arg(long, value_enum, default_value_t = CliVerificationProfile::Quick)]
    profile: CliVerificationProfile,
}

impl VerifyArgs {
    fn into_request(self, command: CommandName) -> CommandRequest {
        let mut request = self.output.into_request(command);

        if let Some(planner_request) = self.selection.into_planner_request() {
            request = request.with_planner_request(planner_request);
        }

        request.with_verification_profile(self.profile.into_verification_profile())
    }
}

#[derive(Debug, Clone, Args)]
pub struct VerifySelectionArgs {
    #[arg(
        long,
        value_name = "ITEM",
        conflicts_with_all = ["bundle", "all"],
        help = "Select one catalog item"
    )]
    item: Option<String>,
    #[arg(
        long,
        value_name = "BUNDLE",
        conflicts_with_all = ["item", "all"],
        help = "Select one catalog bundle"
    )]
    bundle: Option<String>,
    #[arg(
        long,
        conflicts_with_all = ["item", "bundle"],
        help = "Select every catalog item"
    )]
    all: bool,
}

impl VerifySelectionArgs {
    fn into_planner_request(self) -> Option<PlannerRequest> {
        if self.all {
            Some(PlannerRequest::all_items())
        } else if let Some(item) = self.item {
            Some(PlannerRequest::item(item))
        } else {
            self.bundle.map(PlannerRequest::bundle)
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct InstallArgs {
    #[command(flatten)]
    workflow: WorkflowArgs,
    #[arg(long, help = "Preview commands without executing them")]
    dry_run: bool,
}

impl InstallArgs {
    fn into_request(self, command: CommandName) -> CommandRequest {
        let install_mode = if self.dry_run {
            InstallMode::DryRun
        } else {
            InstallMode::Apply
        };

        self.workflow
            .into_request(command)
            .with_install_mode(install_mode)
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum CliVerificationProfile {
    Quick,
    Standard,
    Strict,
}

impl CliVerificationProfile {
    fn into_verification_profile(self) -> VerificationProfile {
        match self {
            Self::Quick => VerificationProfile::Quick,
            Self::Standard => VerificationProfile::Standard,
            Self::Strict => VerificationProfile::Strict,
        }
    }
}
