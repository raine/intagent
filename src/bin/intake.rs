#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let log_path = intake::cli::tracing_log_path(&args);
    let tracing = match intake::application_log::initialize_tracing(log_path.as_deref()) {
        Ok(tracing) => tracing,
        Err(error) => {
            eprintln!("intake: {error}");
            std::process::exit(1);
        }
    };
    if let Some(warning) = tracing.warning {
        tracing::warn!(target: "intake::terminal::error", "Warning: {warning}");
    }
    tracing::info!(
        target: "intake::lifecycle",
        executable = "intake",
        pid = std::process::id(),
        "process started"
    );

    match intake::cli::run(args).await {
        Ok(code) if code != 0 => {
            tracing::warn!(
                target: "intake::lifecycle",
                executable = "intake",
                exit_code = code,
                "process stopped"
            );
            std::process::exit(code);
        }
        Ok(_) => {
            tracing::info!(
                target: "intake::lifecycle",
                executable = "intake",
                exit_code = 0,
                "process stopped"
            );
        }
        Err(error) => {
            let category = intake::dashboard::public_error(Some(&error.to_string()))
                .unwrap_or_else(|| "Operation failed".into());
            tracing::error!(
                target: "intake::lifecycle",
                executable = "intake",
                error_category = category,
                exit_code = 1,
                "process stopped"
            );
            intake::cli::write_error(&error);
            std::process::exit(1);
        }
    }
}
