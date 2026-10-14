fn main() {
    #[cfg(target_os = "linux")]
    let result = match (std::env::args().nth(1), std::env::args().nth(2)) {
        (Some(instance), Some(cgroups)) => {
            hosted_execution::task_service::serve(&instance, std::path::Path::new(&cgroups))
        }
        _ => Err("usage: environment_task_service <instanceId> <delegatedCgroupRoot>".to_string()),
    };
    #[cfg(not(target_os = "linux"))]
    let result: Result<(), String> =
        Err("environment task service requires Linux cgroup v2".into());
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
