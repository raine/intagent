use intake::protocol::{ProtocolError, source_main};

#[tokio::main]
async fn main() {
    let success = source_main(|_| async {
        Err(ProtocolError::SourceUnavailable(
            "Fastmail HTTP polling belongs to the source implementation",
        ))
    })
    .await;
    if !success {
        std::process::exit(1);
    }
}
