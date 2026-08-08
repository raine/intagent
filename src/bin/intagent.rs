#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let log_path = intagent::cli::tracing_log_path(&args);
    let tracing = match intagent::application_log::initialize_tracing(log_path.as_deref()) {
        Ok(tracing) => tracing,
        Err(error) => {
            eprintln!("intagent: {error}");
            std::process::exit(1);
        }
    };
    if let Some(warning) = tracing.warning {
        tracing::warn!(target: "intagent::terminal::error", "Warning: {warning}");
    }
    tracing::info!(
        target: "intagent::lifecycle",
        executable = "intagent",
        pid = std::process::id(),
        "process started"
    );

    match intagent::cli::run(args).await {
        Ok(code) if code != 0 => {
            tracing::warn!(
                target: "intagent::lifecycle",
                executable = "intagent",
                exit_code = code,
                "process stopped"
            );
            std::process::exit(code);
        }
        Ok(_) => {
            tracing::info!(
                target: "intagent::lifecycle",
                executable = "intagent",
                exit_code = 0,
                "process stopped"
            );
        }
        Err(error) => {
            let category = intagent::dashboard::public_error(Some(&error.to_string()))
                .unwrap_or_else(|| "Operation failed".into());
            tracing::error!(
                target: "intagent::lifecycle",
                executable = "intagent",
                error_category = category,
                exit_code = 1,
                "process stopped"
            );
            intagent::cli::write_error(&error);
            std::process::exit(1);
        }
    }
}
