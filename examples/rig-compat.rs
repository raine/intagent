use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use intake::agent::auth::{AuthPaths, authorize, chatgpt_client};
use rig_core::client::CompletionClient;

#[tokio::main]
async fn main() -> Result<()> {
    let mut login = false;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--login" => login = true,
            "--help" | "-h" => {
                println!("Usage: cargo run --example rig-compat -- [--login]");
                return Ok(());
            }
            _ => bail!("unknown argument: {argument}"),
        }
    }

    let config_home = config_home()?;
    let paths = AuthPaths::under_config_home(config_home);
    paths.prepare()?;

    let client = chatgpt_client(&paths.cache, login)?;
    let _model = client.completion_model("gpt-5.6-luna");
    authorize(&paths, login).await?;

    if login {
        println!("ChatGPT subscription login succeeded.");
    } else {
        println!("ChatGPT subscription authentication is available.");
    }
    Ok(())
}

fn config_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(path.into());
    }
    let home = std::env::var_os("HOME").context("HOME is unavailable")?;
    Ok(PathBuf::from(home).join(".config"))
}
