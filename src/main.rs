use clap::Parser;

use envira::{app::App, cli::Cli, error::Result};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let app = App::default();
    let exit_code = app.run(cli.into_request())?;

    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}
