use std::fmt::Write;

use crate::{
    engine::{
        CommandErrorResponse, CommandName, CommandPayload, CommandRequest, CommandResponse, Engine,
        InterfaceMode, OutputFormat,
    },
    error::Result,
    tui,
};

#[derive(Debug, Default)]
pub struct App {
    engine: Engine,
}

impl App {
    pub fn run(&self, request: CommandRequest) -> Result<i32> {
        if request.command == CommandName::Tui {
            tui::run(&self.engine)?;
            return Ok(0);
        }

        let command = request.command;
        let mode = request.mode;
        let format = request.format;

        match self.engine.execute(request) {
            Ok(response) => {
                let exit_code = response.exit_code();
                self.run_headless(response)?;
                Ok(exit_code)
            }
            Err(error) => {
                let response =
                    CommandErrorResponse::new(command, mode, format, error.into_envelope());

                match mode {
                    InterfaceMode::Headless => self.run_headless_error(response)?,
                    InterfaceMode::Tui => eprintln!("{}", render_error_response(&response)),
                }

                Ok(1)
            }
        }
    }

    fn run_headless(&self, response: CommandResponse) -> Result<()> {
        match response.format {
            OutputFormat::Json => println!("{}", response.as_json()?),
            OutputFormat::Text => println!("{}", render_command_response(&response)),
        }

        Ok(())
    }

    fn run_headless_error(&self, response: CommandErrorResponse) -> Result<()> {
        match response.format {
            OutputFormat::Json => println!("{}", response.as_json()?),
            OutputFormat::Text => println!("{}", render_error_response(&response)),
        }

        Ok(())
    }
}

fn render_command_response(response: &CommandResponse) -> String {
    match &response.payload {
        CommandPayload::Catalog { catalog } => format!(
            "Catalog loaded with {} items across {} bundles; {} default bundle(s) are available.",
            catalog.items.len(),
            catalog.bundles.len(),
            catalog.default_bundles.len()
        ),
        CommandPayload::Plan { action_plan } => {
            let mut output = String::new();
            let install_steps = action_plan
                .steps
                .iter()
                .filter(|step| matches!(step.action, crate::planner::PlannedAction::Install))
                .count();
            let repair_steps = action_plan
                .steps
                .iter()
                .filter(|step| matches!(step.action, crate::planner::PlannedAction::Repair))
                .count();
            let skip_steps = action_plan
                .steps
                .iter()
                .filter(|step| matches!(step.action, crate::planner::PlannedAction::Skip))
                .count();
            let blocked_steps = action_plan
                .steps
                .iter()
                .filter(|step| matches!(step.action, crate::planner::PlannedAction::Blocked))
                .count();
            let _ = writeln!(
                output,
                "Planned {} catalog item(s): {install_steps} install, {repair_steps} repair, {skip_steps} skip, {blocked_steps} blocked.",
                action_plan.steps.len()
            );

            for step in &action_plan.steps {
                let _ = writeln!(
                    output,
                    "- {} => {:?}: {}",
                    step.step.item_id, step.action, step.rationale.summary
                );
            }

            output.trim_end().to_string()
        }
        CommandPayload::Verify { verification } => {
            let mut output = String::new();
            let _ = writeln!(
                output,
                "Verified {} catalog item(s); {} met the requested threshold and {} did not.",
                verification.summary.total_steps,
                verification.summary.threshold_met_steps,
                verification.summary.threshold_unmet_steps
            );

            for result in &verification.results {
                let _ = writeln!(
                    output,
                    "- {} => threshold_met={} health={:?}",
                    result.step.item_id, result.result.threshold_met, result.result.health
                );
            }

            output.trim_end().to_string()
        }
        CommandPayload::Install { install } => {
            let mut output = String::new();
            let _ = writeln!(
                output,
                "Install finished with {:?}; execution succeeded={} and {} of {} actionable catalog item(s) met the requested threshold.",
                install.outcome.status,
                install.outcome.execution_succeeded,
                install.outcome.threshold_met_steps,
                install.outcome.actionable_steps
            );

            for failure in &install.outcome.failures {
                let _ = writeln!(
                    output,
                    "- {} => {:?}, threshold_met={}, health={:?}, execution={:?}",
                    failure.item_id,
                    failure.action,
                    failure.verifier.threshold_met,
                    failure.verifier.health,
                    failure.execution_disposition
                );
            }

            output.trim_end().to_string()
        }
    }
}

fn render_error_response(response: &CommandErrorResponse) -> String {
    response.render_text()
}
