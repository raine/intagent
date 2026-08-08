use intake::protocol::source_main;
use intake::sources::{fastmail::poll_fastmail, http_client};

#[tokio::main]
async fn main() {
    if std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("Usage: intake-fastmail-source < poll-request.json");
        return;
    }

    let success = source_main(|request| async move {
        let token = std::env::var("FASTMAIL_API_TOKEN").map_err(|_| {
            intake::protocol::ProtocolError::Source("FASTMAIL_API_TOKEN is required".into())
        })?;
        let client = http_client()?;
        poll_fastmail(request, &client, &token).await
    })
    .await;
    if !success {
        std::process::exit(1);
    }
}
