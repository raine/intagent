use intagent::protocol::source_main;
use intagent::sources::{github::poll_github, http_client};

#[tokio::main]
async fn main() {
    let path = intagent::application_log::source_application_log_path();
    let tracing = match intagent::application_log::initialize_tracing(path.as_deref()) {
        Ok(tracing) => tracing,
        Err(error) => {
            eprintln!("intagent-github-source: {error}");
            std::process::exit(1);
        }
    };
    if let Some(warning) = tracing.warning {
        tracing::warn!(target: "intagent::terminal::error", "Warning: {warning}");
    }
    if std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("Usage: intagent-github-source < poll-request.json");
        return;
    }

    tracing::info!(
        target: "intagent::source",
        source = "github",
        pid = std::process::id(),
        "source poll started"
    );
    let success = source_main(|request| async move {
        let token = std::env::var("GITHUB_TOKEN").map_err(|_| {
            intagent::protocol::ProtocolError::Source("GITHUB_TOKEN is required".into())
        })?;
        let client = http_client()?;
        poll_github(request, &client, &token).await
    })
    .await;
    if success {
        tracing::info!(target: "intagent::source", source = "github", "source poll succeeded");
    } else {
        tracing::error!(target: "intagent::source", source = "github", "source poll failed");
        std::process::exit(1);
    }
}
