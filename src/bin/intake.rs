#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect();
    match intake::cli::run(args).await {
        Ok(code) if code != 0 => std::process::exit(code),
        Ok(_) => {}
        Err(error) => {
            intake::cli::write_error(&error);
            std::process::exit(1);
        }
    }
}
