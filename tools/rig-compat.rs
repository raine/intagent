use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use intagent::agent::auth::{AuthPaths, authorize, chatgpt_client};
use intagent::agent::context::CompactionConfig;
use intagent::agent::model::ThinkingLevel;
use intagent::agent::rig_runner::{ExplicitDriver, ProviderRetryPolicy};
use intagent::agent::telemetry::{CancellationTelemetry, PrototypeTelemetry};
use intagent::agent::tools::RecordingExecutableTools;
use rig_core::client::CompletionClient;
use rig_core::message::Message;
use tokio_util::sync::CancellationToken;

const MODEL_ID: &str = "gpt-5.6-luna";

#[tokio::main]
async fn main() -> Result<()> {
    let mut login = false;
    let mut live_fixture = false;
    let mut fixture_directory = None;
    let mut recording_executable = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--login" => login = true,
            "--live-fixture" => live_fixture = true,
            "--fixture-directory" => {
                fixture_directory = Some(PathBuf::from(
                    arguments
                        .next()
                        .context("--fixture-directory requires a path")?,
                ));
            }
            "--recording-executable" => {
                recording_executable = Some(PathBuf::from(
                    arguments
                        .next()
                        .context("--recording-executable requires a path")?,
                ));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo run --example rig-compat -- [--login | --live-fixture --fixture-directory PATH --recording-executable PATH]"
                );
                return Ok(());
            }
            _ => bail!("unknown argument: {argument}"),
        }
    }
    if login && live_fixture {
        bail!("--login and --live-fixture are separate operations");
    }

    let config_home = config_home()?;
    let paths = AuthPaths::under_config_home(config_home);
    paths.prepare()?;

    let client = chatgpt_client(&paths.cache, login)?;
    if live_fixture {
        authorize(&paths, false).await?;
        return run_live_fixture(
            client,
            fixture_directory.context("--live-fixture requires --fixture-directory")?,
            recording_executable.context("--live-fixture requires --recording-executable")?,
        )
        .await;
    }

    let _model = client.completion_model(MODEL_ID);
    authorize(&paths, login).await?;
    if login {
        println!("ChatGPT subscription login succeeded.");
    } else {
        println!("ChatGPT subscription authentication is available.");
    }
    Ok(())
}

async fn run_live_fixture(
    client: rig_core::providers::chatgpt::Client,
    fixture_directory: PathBuf,
    recording_executable: PathBuf,
) -> Result<()> {
    let fixture_directory = fixture_directory
        .canonicalize()
        .context("canonicalize fixture directory")?;
    let temporary_root = std::env::temp_dir()
        .canonicalize()
        .context("canonicalize temporary directory")?;
    if !fixture_directory.starts_with(&temporary_root) {
        bail!(
            "fixture directory must live under {}",
            temporary_root.display()
        );
    }
    let recording_executable = recording_executable
        .canonicalize()
        .context("canonicalize recording executable")?;
    require_contained(&fixture_directory, &recording_executable)?;
    let database = fixture_directory.join("telemetry.sqlite");
    if database.exists() {
        bail!(
            "temporary telemetry database already exists: {}",
            database.display()
        );
    }

    let telemetry = PrototypeTelemetry::open(&database)?;
    let tools = RecordingExecutableTools::new(recording_executable)?;
    let model = client.completion_model(MODEL_ID);
    let cancellation = CancellationToken::new();
    let signal_cancellation = cancellation.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });
    let driver = ExplicitDriver::new(
        &model,
        &tools,
        telemetry.clone(),
        CancellationTelemetry::default(),
        cancellation,
        "This is an isolated compatibility fixture. Call the bash tool exactly once with {\"value\":\"allowed\"}. After its result, reply with `fixture complete`. Never communicate externally.",
        ThinkingLevel::Max,
        ProviderRetryPolicy::default(),
        CompactionConfig {
            trigger_tokens: 1,
            keep_recent_groups: 1,
            max_compactions: 1,
        },
    );
    let response = driver
        .run(
            "Run the isolated recording fixture.",
            vec![
                Message::user("Compatibility history item one."),
                Message::assistant("Compatibility history item two."),
            ],
            2,
        )
        .await?;
    if tools.executions() != 1 {
        bail!("live fixture did not execute exactly one recording tool call");
    }
    let retry_count = telemetry.retry_rows()?.len();
    let compactions = telemetry.compaction_rows()?;
    if compactions.len() != 1 || compactions[0].outcome != "completed" {
        bail!("live fixture did not complete exactly one context compaction");
    }
    println!(
        "Live compatibility fixture succeeded with {retry_count} provider retries and one compaction. Telemetry: {}. Output: {}",
        database.display(),
        response.output
    );
    Ok(())
}

fn require_contained(root: &Path, path: &Path) -> Result<()> {
    if path.starts_with(root) {
        Ok(())
    } else {
        bail!("recording executable must live inside the fixture directory")
    }
}

fn config_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(path.into());
    }
    let home = std::env::var_os("HOME").context("HOME is unavailable")?;
    Ok(PathBuf::from(home).join(".config"))
}
