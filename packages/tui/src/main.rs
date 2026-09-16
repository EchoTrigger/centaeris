mod app;
mod clipboard;
mod runtime_client;
mod tool_projection;
#[cfg(windows)]
mod wsl;

fn main() {
    if let Err(error) = app::run() {
        eprintln!("centa failed: {error}");
        std::process::exit(1);
    }
}
