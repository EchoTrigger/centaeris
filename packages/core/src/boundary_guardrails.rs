use std::fs;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref())
        .unwrap_or_else(|err| panic!("read {} failed: {err}", path.as_ref().display()))
}

fn read_src(path: &str) -> String {
    read_repo_file(manifest_dir().join(path))
}

fn collect_rs_files(root: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rs_files_inner(root.as_ref(), &mut files);
    files.sort();
    files
}

fn collect_rs_files_inner(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().and_then(|item| item.to_str()) == Some("rs") {
            files.push(path.to_path_buf());
        }
        return;
    }

    for entry in
        fs::read_dir(path).unwrap_or_else(|err| panic!("read dir {} failed: {err}", path.display()))
    {
        let entry = entry.expect("read dir entry");
        collect_rs_files_inner(entry.path().as_path(), files);
    }
}

fn assert_source_does_not_contain(path: &Path, source: &str, forbidden: &str) {
    assert!(
        !source.contains(forbidden),
        "{} must not contain `{}`",
        path.display(),
        forbidden
    );
}

#[test]
fn transcript_projection_public_module_entry_is_canonical() {
    let root = manifest_dir();
    assert!(root.join("src/runtime/projection.rs").is_file());

    let runtime = read_src("src/runtime.rs");
    assert!(runtime.contains("pub mod projection;"));
}

#[test]
fn runtime_uses_facade_files_for_boundary_modules() {
    let root = manifest_dir();
    for facade in [
        "execution",
        "extension",
        "model",
        "runtime",
        "session",
        "tool",
    ] {
        assert!(root.join(format!("src/{facade}.rs")).is_file());
        assert!(!root.join("src").join(facade).join("mod.rs").exists());
    }
}

#[test]
fn transcript_projection_is_not_session_or_model_input() {
    let root = manifest_dir();
    let mut files = vec![
        root.join("src/session.rs"),
        root.join("src/session/manager.rs"),
        root.join("src/runtime/generate_request.rs"),
        root.join("src/runtime/prompt_projection.rs"),
    ];
    files.extend(collect_rs_files(root.join("src/model/prompt")));

    for path in files {
        let source = read_repo_file(path.as_path());
        assert_source_does_not_contain(path.as_path(), source.as_str(), "runtime::projection");
    }
}

#[test]
fn core_public_modules_are_the_six_framework_facades() {
    let lib = read_src("src/lib.rs");
    let public_modules = lib
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("pub mod ")
                .and_then(|line| line.strip_suffix(';'))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        public_modules,
        [
            "execution",
            "extension",
            "model",
            "runtime",
            "session",
            "tool"
        ]
    );
}

#[test]
fn core_has_no_production_adapter_dependencies() {
    let manifest = read_src("Cargo.toml");
    let mut section = "";
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed;
            continue;
        }
        let production_dependencies = section == "[dependencies]"
            || (section.starts_with("[target.") && section.ends_with(".dependencies]"));
        if production_dependencies {
            for dependency in ["rusqlite", "postgres", "windows-sys", "libc", "fs2", "zstd"] {
                assert!(
                    !trimmed.starts_with(format!("{dependency}.").as_str())
                        && !trimmed.starts_with(format!("{dependency} =").as_str()),
                    "Core production dependency must stay adapter-free: {dependency}"
                );
            }
        }
    }
}

#[test]
fn core_source_paths_do_not_reach_host_implementations() {
    for path in collect_rs_files(manifest_dir().join("src")) {
        let source = read_repo_file(path.as_path());
        for line in source.lines().filter(|line| line.contains("#[path")) {
            assert!(
                !line.contains("hosts/") && !line.contains("hosts\\"),
                "Core source path must not include a Host implementation: {}: {line}",
                path.display()
            );
        }
    }
}
