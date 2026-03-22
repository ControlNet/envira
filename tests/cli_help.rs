use std::process::Command;

fn run_help(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_envira"))
        .args(arguments)
        .output()
        .expect("binary should run")
}

#[test]
fn top_level_help_lists_scaffolded_commands() {
    let output = run_help(&["--help"]);
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid utf-8");

    for command in ["catalog", "plan", "install", "verify", "tui"] {
        assert!(
            stdout.contains(command),
            "expected top-level help to include {command}, got:\n{stdout}"
        );
    }
}

#[test]
fn subcommand_help_exits_successfully() {
    for command in ["catalog", "verify", "tui"] {
        let output = run_help(&[command, "--help"]);
        assert!(
            output.status.success(),
            "expected `{command} --help` to exit successfully"
        );
    }
}
