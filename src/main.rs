fn main() {
    let result =
        howto::engine_runtime::run_worker_from_env().unwrap_or_else(howto::cli::run_from_env);
    match result {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            let message = howto::error::terminal_safe_message(&error.to_string());
            eprintln!("error: {message}");
            std::process::exit(error.exit_code());
        }
    }
}
