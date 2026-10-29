mod app;
mod catalog;
mod cli;
mod commands;
mod exec;
mod facts;
mod fetch;
mod fs;
mod latest;
mod plan;
mod runner;
mod shellenv;
mod state;
mod status;
mod template;
mod tui;
mod version;

use clap::Parser;

use cli::{Cli, Command, ConfigCmd};

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("envira: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    // Commands called from templates run without a catalog or async runtime.
    match &cli.command {
        Some(Command::Fs { cmd }) => return commands::fs_cmd(cmd),
        Some(Command::FetchRelease {
            repo,
            assets,
            bins,
            to,
            tag,
        }) => {
            fetch::run(&fetch::FetchArgs {
                repo: repo.clone(),
                assets: assets.clone(),
                bins: bins.clone(),
                to: fs::expand(&to.display().to_string()),
                tag: tag.clone(),
            })?;
            return Ok(0);
        }
        Some(Command::Catalog { cmd }) => return commands::catalog_cmd(&cli.global, cmd),
        _ => {}
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let sess = app::Session::open(&cli.global)?;
        match cli.command {
            None if std::io::IsTerminal::is_terminal(&std::io::stdout()) => tui::run(sess).await,
            None => commands::status(&sess, false, false, false).await,
            Some(Command::Status {
                json,
                latest,
                refresh,
            }) => commands::status(&sess, json, latest || refresh, refresh).await,
            Some(Command::Update {
                items,
                check,
                refresh,
                offline,
                run,
            }) => commands::update(&sess, &items, check, refresh, offline, &run).await,
            Some(Command::Adopt { items, dry_run }) => {
                commands::adopt(&sess, &items, dry_run).await
            }
            Some(Command::Sync { bundles, run }) => commands::sync(&sess, &bundles, &run).await,
            Some(Command::Install { items, force, run }) => {
                commands::install(&sess, &items, force, &run).await
            }
            Some(Command::Retry { run }) => commands::retry(&sess, &run).await,
            Some(Command::Config {
                cmd: ConfigCmd::Apply { items, run },
            }) => commands::config(&sess, &items, false, &run).await,
            Some(Command::Config {
                cmd: ConfigCmd::Revert { items, run },
            }) => commands::config(&sess, &items, true, &run).await,
            Some(Command::Plan { items, script }) => {
                commands::plan_cmd(&sess, &items, script).await
            }
            Some(Command::Env) => commands::env_cmd(&sess).await,
            Some(Command::Log { item, run }) => {
                commands::log(&sess, item.as_deref(), run.as_deref())
            }
            Some(Command::Doctor) => commands::doctor(&sess).await,
            Some(Command::Fs { .. } | Command::FetchRelease { .. } | Command::Catalog { .. }) => {
                unreachable!()
            }
        }
    })
}
