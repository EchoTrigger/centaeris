use std::{fs, path::PathBuf, process::Command};

fn runtime_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn runtime_methods_have_one_declarative_rust_source() {
    let root = runtime_root();
    let registry = fs::read_to_string(root.join("src/runtime_command_registry.rs"))
        .expect("runtime command registry source");
    let host_protocol =
        fs::read_to_string(root.join("src/host_protocol.rs")).expect("host protocol source");
    let commands = fs::read_to_string(root.join("src/commands.rs")).expect("command source");

    assert!(registry.contains("runtime_commands"));
    assert!(!host_protocol.contains("SHARED_RUNTIME_COMMANDS"));
    assert!(!host_protocol.contains("EXECUTION_HOST_COMMANDS"));
    assert!(!host_protocol.contains("HOST_SURFACE_COMMANDS"));
    assert!(!commands.contains("const LOCAL_RUNTIME_COMMANDS"));
    assert!(!commands.contains("\"session/prompt\" =>"));
}

#[test]
fn generated_runtime_protocol_reference_is_current() {
    let binary = env!("CARGO_BIN_EXE_centaeris-runtime-protocol-docs");
    let output = Command::new(binary)
        .arg("--check")
        .output()
        .expect("run runtime protocol documentation generator");

    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn desktop_parity_consumes_the_generated_runtime_registry() {
    let root = runtime_root();
    assert!(root.join("generated/runtime-methods.json").is_file());

    let parity = fs::read_to_string(root.join("../desktop/scripts/check-host-parity.mjs"))
        .expect("Desktop Host parity script");
    assert!(parity.contains("packages/runtime/generated/runtime-methods.json"));
    assert!(!parity.contains("packages/runtime/src/commands.rs"));
}

#[test]
fn local_ci_checks_generated_runtime_protocol_reference() {
    let ci =
        fs::read_to_string(runtime_root().join("../../scripts/ci.ps1")).expect("local CI script");
    assert!(ci.contains(
        "cargo run --locked -p centaeris-runtime --bin centaeris-runtime-protocol-docs -- --check"
    ));
}
