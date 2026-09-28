mod app;
mod automation_cli;
mod clipboard;
mod runtime_client;
mod tool_projection;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !args.is_empty() {
        match automation_cli::run(&args) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    if let Err(error) = app::run() {
        eprintln!("centa failed: {error}");
        std::process::exit(1);
    }
}
