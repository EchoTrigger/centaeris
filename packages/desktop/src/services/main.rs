fn main() {
    match centaeris_desktop_services::dispatch_execution_helper() {
        Ok(Some(exit_code)) => std::process::exit(exit_code),
        Ok(None) => {}
        Err(error) => {
            eprintln!(
                "centaeris Windows host launcher failed: {}",
                error.message()
            );
            std::process::exit(1);
        }
    }
    if let Err(error) = centaeris_desktop_services::run_host() {
        eprintln!("centaeris Runtime Host failed: {}", error);
        std::process::exit(1);
    }
}
