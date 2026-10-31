mod app;
mod apt;
mod catalog;
mod cli;
mod commands;
mod diagnose;
mod exec;
mod facts;
mod fetch;
mod fs;
mod latest;
mod net;
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
    if reads_only(&cli.command) {
        // SAFETY: called before any thread starts; restores the default action.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    let code = match run(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("envira: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

/// Read-only commands end quietly when the reader goes away (`envira doctor | head`), as other
/// Unix tools do; Rust ignores SIGPIPE, so printing would panic instead. Commands that change the
/// machine keep ignoring it: a closed pipe must not kill an install halfway.
fn reads_only(c: &Option<Command>) -> bool {
    matches!(
        c,
        None | Some(
            Command::Status { .. }
                | Command::Plan { .. }
                | Command::Log { .. }
                | Command::Explain { .. }
                | Command::Doctor
                | Command::Catalog { .. }
        )
    )
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
            install_with,
        }) => {
            fetch::run(&fetch::FetchArgs {
                repo: repo.clone(),
                assets: assets.clone(),
                bins: bins.clone(),
                to: fs::expand(&to.display().to_string()),
                tag: tag.clone(),
                install_with: install_with.split_whitespace().map(String::from).collect(),
            })?;
            return Ok(0);
        }
        Some(Command::AptRefresh { max_age }) => {
            let max_age = max_age.map_or(apt::MAX_AGE, std::time::Duration::from_secs);
            return apt::run(&apt::Paths::default(), max_age);
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
            Some(Command::Log { list: true, .. }) => commands::log_list(&sess),
            Some(Command::Log { item, run, .. }) => {
                commands::log(&sess, item.as_deref(), run.as_deref())
            }
            Some(Command::Doctor) => commands::doctor(&sess).await,
            Some(Command::Explain { item }) => commands::explain(&sess, &item).await,
            Some(
                Command::Fs { .. }
                | Command::FetchRelease { .. }
                | Command::AptRefresh { .. }
                | Command::Catalog { .. },
            ) => {
                unreachable!()
            }
        }
    })
}
