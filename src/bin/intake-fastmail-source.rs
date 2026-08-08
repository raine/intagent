use intake::protocol::source_main;
use intake::sources::{fastmail::poll_fastmail, http_client};

#[tokio::main]
async fn main() {
    let directory = intake::application_log::source_application_log_directory();
    let tracing = match intake::application_log::initialize_tracing(
        "intake-fastmail-source",
        directory.as_deref(),
    ) {
        Ok(tracing) => tracing,
        Err(error) => {
            eprintln!("intake-fastmail-source: {error}");
            std::process::exit(1);
        }
    };
    if let Some(warning) = tracing.warning {
        tracing::warn!(target: "intake::terminal::error", "Warning: {warning}");
    }
    if std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("Usage: intake-fastmail-source < poll-request.json");
        return;
    }

    tracing::info!(
        target: "intake::source",
        source = "fastmail",
        pid = std::process::id(),
        "source poll started"
    );
    let success = source_main(|request| async move {
        let token = std::env::var("FASTMAIL_API_TOKEN").map_err(|_| {
            intake::protocol::ProtocolError::Source("FASTMAIL_API_TOKEN is required".into())
        })?;
        let client = http_client()?;
        poll_fastmail(request, &client, &token).await
    })
    .await;
    if success {
        tracing::info!(target: "intake::source", source = "fastmail", "source poll succeeded");
    } else {
        tracing::error!(target: "intake::source", source = "fastmail", "source poll failed");
        std::process::exit(1);
    }
}
