use std::{env, fs, path::PathBuf, process::ExitCode};

#[path = "../runtime_command_registry.rs"]
mod runtime_command_registry;

use runtime_command_registry::{runtime_commands, RuntimeOperationKind, RuntimeRetryPolicy};

const START_MARKER: &str = "<!-- BEGIN GENERATED:RUNTIME_METHOD_REGISTRY -->";
const END_MARKER: &str = "<!-- END GENERATED:RUNTIME_METHOD_REGISTRY -->";

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandScope {
    SharedRuntime,
    ExecutionHost,
    HostSurface,
}

impl CommandScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SharedRuntime => "sharedRuntime",
            Self::ExecutionHost => "executionHost",
            Self::HostSurface => "hostSurface",
        }
    }
}

struct CommandDescriptor {
    command: &'static str,
    scope: CommandScope,
    operation_kind: RuntimeOperationKind,
    retry_policy: RuntimeRetryPolicy,
    reconcile_method: Option<&'static str>,
}

macro_rules! define_command_descriptors {
    ($( $variant:ident, $command:literal, $scope:ident, $operation_kind:ident, $retry_policy:ident, $reconcile_method:expr; )*) => {
        const COMMANDS: &[CommandDescriptor] = &[
            $(
                CommandDescriptor {
                    command: $command,
                    scope: CommandScope::$scope,
                    operation_kind: RuntimeOperationKind::$operation_kind,
                    retry_policy: RuntimeRetryPolicy::$retry_policy,
                    reconcile_method: $reconcile_method,
                },
            )*
        ];
    };
}

runtime_commands!(define_command_descriptors);

fn protocol_reference_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/RuntimeProtocol.md")
}

fn protocol_registry_manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("generated/runtime-methods.json")
}

fn generated_registry() -> String {
    let mut output = String::from(START_MARKER);
    output.push_str(
        "\n\n`operationKind` classifies the observable effect: `read`, \
         `desiredStateWrite`, `identityMutation`, `creation`, or \
         `oneShotAction`. `retryPolicy` is one of `safeRetry`, \
         `sameOperationId`, or `noAutomaticRetry`. A non-empty \
         `reconcileMethod` names the registered method that observes or \
         resumes an uncertain outcome.\n\n",
    );
    for (scope, heading) in [
        (CommandScope::SharedRuntime, "Shared Runtime"),
        (CommandScope::ExecutionHost, "Execution Host"),
        (CommandScope::HostSurface, "Native Host surface"),
    ] {
        output.push_str("### ");
        output.push_str(heading);
        output.push_str(
            "\n\n| Method | Operation kind | Retry policy | Reconcile method |\n\
             | --- | --- | --- | --- |\n",
        );
        for descriptor in COMMANDS
            .iter()
            .filter(|descriptor| descriptor.scope == scope)
        {
            output.push_str("| `");
            output.push_str(descriptor.command);
            output.push_str("` | `");
            output.push_str(descriptor.operation_kind.as_str());
            output.push_str("` | `");
            output.push_str(descriptor.retry_policy.as_str());
            output.push_str("` | ");
            match descriptor.reconcile_method {
                Some(method) => {
                    output.push('`');
                    output.push_str(method);
                    output.push('`');
                }
                None => output.push('—'),
            }
            output.push_str(" |\n");
        }
        output.push('\n');
    }
    output.push_str(END_MARKER);
    output
}

fn generated_registry_manifest() -> Result<String, String> {
    let methods = COMMANDS
        .iter()
        .map(|descriptor| {
            serde_json::json!({
                "name": descriptor.command,
                "operationKind": descriptor.operation_kind.as_str(),
                "reconcileMethod": descriptor.reconcile_method,
                "retryPolicy": descriptor.retry_policy.as_str(),
                "scope": descriptor.scope.as_str(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&serde_json::json!({
        "schema": "centaeris.runtime-method-registry.v1",
        "methods": methods,
    }))
    .map(|json| format!("{json}\n"))
    .map_err(|error| format!("serialize Runtime method registry failed: {error}"))
}

fn replace_generated_registry(document: &str) -> Result<String, String> {
    let document = document.replace("\r\n", "\n");
    let start = document
        .find(START_MARKER)
        .ok_or_else(|| format!("missing generated registry marker: {START_MARKER}"))?;
    let end_start = document
        .find(END_MARKER)
        .ok_or_else(|| format!("missing generated registry marker: {END_MARKER}"))?;
    if end_start <= start {
        return Err("runtime protocol registry markers are out of order".to_string());
    }
    let end = end_start + END_MARKER.len();
    Ok(format!(
        "{}{}{}",
        &document[..start],
        generated_registry(),
        &document[end..]
    ))
}

fn run() -> Result<(), String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let mode = arguments
        .first()
        .ok_or_else(|| "usage: centaeris-runtime-protocol-docs (--check|--write)".to_string())?;
    if arguments.len() != 1 {
        return Err("runtime protocol documentation generator accepts one argument".to_string());
    }
    let reference_path = protocol_reference_path();
    let current_reference = fs::read_to_string(&reference_path)
        .map_err(|error| format!("read {} failed: {error}", reference_path.display()))?;
    let updated_reference = replace_generated_registry(&current_reference)?;
    let manifest_path = protocol_registry_manifest_path();
    let updated_manifest = generated_registry_manifest()?;
    match mode.as_str() {
        "--check" => {
            let current_manifest = fs::read_to_string(&manifest_path)
                .map_err(|error| format!("read {} failed: {error}", manifest_path.display()))?;
            let reference_current = current_reference.replace("\r\n", "\n") == updated_reference;
            let manifest_current = current_manifest.replace("\r\n", "\n") == updated_manifest;
            if reference_current && manifest_current {
                Ok(())
            } else {
                Err("generated Runtime protocol artifacts are stale; run centaeris-runtime-protocol-docs --write".to_string())
            }
        }
        "--write" => {
            let manifest_parent = manifest_path
                .parent()
                .ok_or_else(|| "Runtime registry manifest has no parent directory".to_string())?;
            fs::create_dir_all(manifest_parent)
                .map_err(|error| format!("create {} failed: {error}", manifest_parent.display()))?;
            fs::write(&reference_path, updated_reference)
                .map_err(|error| format!("write {} failed: {error}", reference_path.display()))?;
            fs::write(&manifest_path, updated_manifest)
                .map_err(|error| format!("write {} failed: {error}", manifest_path.display()))
        }
        _ => Err(format!("unsupported generator mode: {mode}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        generated_registry, generated_registry_manifest, replace_generated_registry, COMMANDS,
    };

    #[test]
    fn generated_registry_contains_every_command_once() {
        let generated = generated_registry();
        for descriptor in COMMANDS {
            assert_eq!(
                generated
                    .lines()
                    .filter(|line| line.starts_with(&format!("| `{}` |", descriptor.command)))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn generated_registry_replaces_only_its_marked_region() {
        let document = "before\n<!-- BEGIN GENERATED:RUNTIME_METHOD_REGISTRY -->\nstale\n<!-- END GENERATED:RUNTIME_METHOD_REGISTRY -->\nafter\n";
        let updated = replace_generated_registry(document).expect("replace generated registry");
        assert!(updated.starts_with("before\n"));
        assert!(updated.ends_with("\nafter\n"));
        assert!(!updated.contains("stale"));
    }

    #[test]
    fn generated_manifest_is_strict_and_complete() {
        let manifest = generated_registry_manifest().expect("generate registry manifest");
        let value: serde_json::Value =
            serde_json::from_str(&manifest).expect("parse registry manifest");
        assert_eq!(value["schema"], "centaeris.runtime-method-registry.v1");
        let methods = value["methods"].as_array().expect("methods array");
        assert_eq!(methods.len(), COMMANDS.len());

        let mut unique_names = std::collections::HashSet::new();
        for method in methods {
            let fields = method.as_object().expect("method descriptor object");
            assert_eq!(
                fields
                    .keys()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>(),
                std::collections::BTreeSet::from([
                    "name",
                    "operationKind",
                    "reconcileMethod",
                    "retryPolicy",
                    "scope",
                ])
            );
            assert!(unique_names.insert(method["name"].as_str().expect("method name")));
            assert!(matches!(
                method["operationKind"].as_str(),
                Some(
                    "read"
                        | "desiredStateWrite"
                        | "identityMutation"
                        | "creation"
                        | "oneShotAction"
                )
            ));
            assert!(matches!(
                method["retryPolicy"].as_str(),
                Some("safeRetry" | "sameOperationId" | "noAutomaticRetry")
            ));
            assert!(
                method["reconcileMethod"].is_null() || method["reconcileMethod"].as_str().is_some()
            );
            if let Some(reconcile_method) = method["reconcileMethod"].as_str() {
                assert!(methods
                    .iter()
                    .any(|candidate| { candidate["name"].as_str() == Some(reconcile_method) }));
            }
        }
    }

    #[test]
    fn retry_classifications_remain_conservative() {
        let same_operation_id = COMMANDS
            .iter()
            .filter(|descriptor| {
                descriptor.retry_policy == super::RuntimeRetryPolicy::SameOperationId
            })
            .map(|descriptor| descriptor.command)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            same_operation_id,
            std::collections::BTreeSet::from(["session/new", "session/prompt"])
        );

        for descriptor in COMMANDS {
            if descriptor.operation_kind == super::RuntimeOperationKind::Read {
                assert_eq!(
                    descriptor.retry_policy,
                    super::RuntimeRetryPolicy::SafeRetry,
                    "read command must be safely retryable: {}",
                    descriptor.command
                );
            } else if !same_operation_id.contains(descriptor.command) {
                assert_eq!(
                    descriptor.retry_policy,
                    super::RuntimeRetryPolicy::NoAutomaticRetry,
                    "unproved mutation must not be automatically retried: {}",
                    descriptor.command
                );
            }
        }
    }

    #[test]
    fn generated_reference_exposes_reliability_metadata() {
        let generated = generated_registry();
        for heading in [
            "| Method | Operation kind | Retry policy | Reconcile method |",
            "| --- | --- | --- | --- |",
        ] {
            assert!(generated.contains(heading));
        }
    }
}
