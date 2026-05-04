use std::process::Command;

fn run_help(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_envira"))
        .args(arguments)
        .output()
        .expect("binary should run")
}

#[test]
fn top_level_help_lists_catalog_driven_commands() {
    let output = run_help(&["--help"][..]);
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid utf-8");
    assert!(stdout.contains("Catalog-driven software environment management tool"));

    for command in ["catalog", "plan", "install", "verify", "tui"] {
        assert!(
            stdout.contains(command),
            "expected top-level help to include {command}, got:\n{stdout}"
        );
    }
}

#[test]
fn subcommand_help_exits_successfully() {
    for command in ["catalog", "plan", "install", "verify", "tui"] {
        let output = run_help(&[command, "--help"][..]);
        assert!(
            output.status.success(),
            "expected `{command} --help` to exit successfully"
        );
    }
}

#[test]
fn workflow_help_lists_selection_modes_and_default_bundle_behavior() {
    for command in ["plan", "install", "verify"] {
        let output = run_help(&[command, "--help"][..]);
        assert!(output.status.success());

        let stdout = String::from_utf8(output.stdout).expect("stdout should be valid utf-8");

        for flag in ["--item <ITEM>", "--bundle <BUNDLE>", "--all"] {
            assert!(
                stdout.contains(flag),
                "expected {command} help to include {flag}, got:\n{stdout}"
            );
        }

        assert!(
            stdout.contains("default_bundles"),
            "expected {command} help to mention default_bundles, got:\n{stdout}"
        );
    }
}

#[test]
fn catalog_help_mentions_default_bundles() {
    let output = run_help(&["catalog", "--help"][..]);
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid utf-8");
    assert!(stdout.contains("default bundles"));
}
