use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cet"))
}

#[test]
fn retired_background_commands_are_rejected_before_runtime_start() {
    for args in [
        &["schedule", "list"][..],
        &["schedule", "service", "start"][..],
        &["ssh", "exec", "fixture-host", "echo fixture"][..],
    ] {
        let output = cli().args(args).output().expect("run retired command");
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("does not accept prompt options")
                || stderr.contains("cet ssh connect <host>"),
            "{stderr}"
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn help_and_version_work_without_starting_a_runtime() {
    let absent_runtime = std::env::temp_dir().join(format!(
        "cet-missing-runtime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for flag in ["--version", "-V"] {
        let output = cli()
            .arg(flag)
            .env("CENTAERIS_RUNTIME_EXE", &absent_runtime)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim_end(),
            concat!("cet ", env!("CARGO_PKG_VERSION"))
        );
    }
    for flag in ["--help", "-h"] {
        let output = cli()
            .arg(flag)
            .env("CENTAERIS_RUNTIME_EXE", &absent_runtime)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("cet [--mode tui] [--workspace <directory>]"));
        assert!(help.contains("cet --mode print|json"));
        assert!(help.contains("cet --mode rpc"));
    }
}

#[test]
fn default_and_explicit_tui_forward_workspace_to_directory_validation() {
    let absent_workspace = std::env::temp_dir().join(format!(
        "cet-missing-workspace-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for arguments in [vec!["--workspace"], vec!["--mode", "tui", "--workspace"]] {
        let output = cli()
            .args(arguments)
            .arg(&absent_workspace)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("cet failed: workspace path is not readable:"));
    }
}
