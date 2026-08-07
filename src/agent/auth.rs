use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use rig_core::providers::chatgpt;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthPaths {
    pub directory: PathBuf,
    pub cache: PathBuf,
}

impl AuthPaths {
    pub fn under_config_home(config_home: impl AsRef<Path>) -> Self {
        let directory = config_home.as_ref().join("intake").join("agent");
        let cache = directory.join("rig-auth.json");
        Self { directory, cache }
    }

    pub fn prepare(&self) -> Result<()> {
        create_private_directory(&self.directory)?;
        match fs::symlink_metadata(&self.cache) {
            Ok(metadata) => validate_cache_metadata(&self.cache, &metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                write_cache_atomically(&self.cache, b"{}\n")?;
            }
            Err(error) => return Err(error).context("inspect Rig authentication cache"),
        }

        let bytes = fs::read(&self.cache).context("read Rig authentication cache")?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).context("parse Rig authentication cache")?;
        if !value.is_object() {
            bail!("Rig authentication cache must contain a JSON object");
        }
        set_private_file_mode(&self.cache)?;
        Ok(())
    }

    pub fn repair_permissions(&self) -> Result<()> {
        validate_cache_path(&self.cache)?;
        set_private_file_mode(&self.cache)
    }
}

pub fn chatgpt_client(auth_file: &Path, interactive: bool) -> Result<chatgpt::Client> {
    Ok(chatgpt::Client::builder()
        .oauth()
        .auth_file(auth_file)
        .originator("intake")
        .user_agent(concat!("intake/", env!("CARGO_PKG_VERSION")))
        .default_instructions("")
        .allow_device_flow(interactive)
        .build()?)
}

pub async fn authorize(paths: &AuthPaths, interactive: bool) -> Result<()> {
    paths.prepare()?;
    let client = chatgpt_client(&paths.cache, interactive)?;
    client.authorize().await.map_err(|error| {
        let message = error.to_string();
        if !interactive && message.contains("sign-in required") {
            anyhow::anyhow!("ChatGPT subscription authentication is required. Run `intake login`.")
        } else {
            anyhow::anyhow!(message)
        }
    })?;
    paths.repair_permissions()
}

pub fn write_cache_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_slice(contents).context("validate Rig authentication cache JSON")?;
    if !value.is_object() {
        bail!("Rig authentication cache must contain a JSON object");
    }

    let parent = path
        .parent()
        .context("Rig authentication cache has no parent directory")?;
    create_private_directory(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        validate_cache_metadata(path, &metadata)?;
    }

    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Rig authentication cache has an invalid file name")?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        set_create_mode(&mut options, 0o600);
        let mut file = options
            .open(&temporary)
            .context("create temporary Rig authentication cache")?;
        file.write_all(contents)
            .context("write temporary Rig authentication cache")?;
        file.sync_all()
            .context("sync temporary Rig authentication cache")?;
        fs::rename(&temporary, path).context("replace Rig authentication cache")?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .context("sync Rig authentication directory")?;
        set_private_file_mode(path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).context("create Rig authentication directory")?;
    let metadata = fs::symlink_metadata(path).context("inspect Rig authentication directory")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("Rig authentication directory must be a real directory");
    }
    validate_owner(path, &metadata)?;
    set_mode(path, 0o700).context("set Rig authentication directory permissions")
}

fn validate_cache_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).context("inspect Rig authentication cache")?;
    validate_cache_metadata(path, &metadata)
}

fn validate_cache_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "Rig authentication cache must be a regular file: {}",
            path.display()
        );
    }
    validate_owner(path, metadata)
}

#[cfg(unix)]
fn validate_owner(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "Rig authentication path must be owned by the current user: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owner(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_create_mode(options: &mut OpenOptions, mode: u32) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(mode);
}

#[cfg(not(unix))]
fn set_create_mode(_options: &mut OpenOptions, _mode: u32) {}

fn set_private_file_mode(path: &Path) -> Result<()> {
    set_mode(path, 0o600).context("set Rig authentication cache permissions")
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}
