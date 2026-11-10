mod app;
mod automation_cli;
mod clipboard;
mod headless;
mod tool_projection;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "schedule" | "ssh"))
    {
        automation_cli::run(&args)
    } else {
        headless::parse(&args).and_then(|invocation| match invocation {
            headless::Invocation::Interactive => {
                app::run().map(|()| 0).map_err(|error| error.to_string())
            }
            headless::Invocation::Prompt(options) => headless::run(options),
            headless::Invocation::Rpc => centaeris_desktop_services::run_rpc_stdio().map(|()| 0),
            headless::Invocation::Help => {
                println!("{}\n\n{}", headless::HELP, automation_cli::HELP);
                Ok(0)
            }
        })
    };
    match result {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("centa failed: {error}");
            std::process::exit(1);
        }
    }
}
