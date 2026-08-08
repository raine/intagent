use intake::protocol::source_main;
use intake::sources::{github::poll_github, http_client};

#[tokio::main]
async fn main() {
    if std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("Usage: intake-github-source < poll-request.json");
        return;
    }

    let success = source_main(|request| async move {
        let token = std::env::var("GITHUB_TOKEN").map_err(|_| {
            intake::protocol::ProtocolError::Source("GITHUB_TOKEN is required".into())
        })?;
        let client = http_client()?;
        poll_github(request, &client, &token).await
    })
    .await;
    if !success {
        std::process::exit(1);
    }
}
