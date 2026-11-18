mod app;
mod arguments;
mod clipboard;
mod headless;
mod ssh;
mod tool_projection;

fn main() {
    match centaeris_desktop_services::dispatch_execution_helper() {
        Ok(Some(code)) => std::process::exit(code),
        Ok(None) => {}
        Err(error) => {
            eprintln!("cet execution helper failed: {error}");
            std::process::exit(1);
        }
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = if args.first().is_some_and(|arg| arg == "ssh") {
        ssh::run(&args[1..])
    } else {
        arguments::parse(&args).and_then(|invocation| match invocation {
            arguments::Invocation::Interactive { workspace } => app::run(workspace)
                .map(|()| 0)
                .map_err(|error| error.to_string()),
            arguments::Invocation::Prompt(options) => headless::run(options),
            arguments::Invocation::Rpc => {
                centaeris_desktop_services::rpc_client::run_embedded_rpc_stdio().map(|()| 0)
            }
            arguments::Invocation::Help => {
                println!("{}\n\n{}", arguments::HELP, ssh::HELP);
                Ok(0)
            }
            arguments::Invocation::Version => {
                println!("cet {}", env!("CARGO_PKG_VERSION"));
                Ok(0)
            }
        })
    };
    match result {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("cet failed: {error}");
            std::process::exit(1);
        }
    }
}
