use std::process::Command;

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
