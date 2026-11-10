fn main() {
    #[cfg(target_os = "linux")]
    let result = match (std::env::args().nth(1), std::env::args().nth(2)) {
        (Some(config), None) => {
            hosted_execution::host_supervisor::serve(std::path::Path::new(&config))
        }
        _ => Err("usage: environment_host_supervisor <config.json>".to_owned()),
    };
    #[cfg(not(target_os = "linux"))]
    let result: Result<(), String> = Err("native environment supervisor requires Linux".into());
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
