use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::agent::process::{ProcessOptions, run_process};
use crate::agent::read_policy::open_absolute_no_follow;
use crate::config::{canonical_roots, emit_yaml, expand_path, is_within, parse_yaml_value};

pub const MAX_PROJECT_REGISTRY_BYTES: usize = 64 * 1024;
pub const MAX_PROJECTS: usize = 1000;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInventoryEntry {
    pub path: PathBuf,
    pub remotes: Vec<String>,
    pub github_repositories: Vec<String>,
    pub default_branch: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectInventory {
    pub projects: Vec<ProjectInventoryEntry>,
    pub diagnostics: Vec<String>,
}

pub fn ensure_project_registry(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => return Ok(()),
        Ok(_) => bail!("project registry path is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = path.parent().context("project registry has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create project registry directory: {}", parent.display()))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let canonical_parent = fs::canonicalize(parent)?;
    let parent_file = open_absolute_no_follow(&canonical_parent, true)?;
    let name = CString::new(
        path.file_name()
            .context("project registry has no file name")?
            .as_bytes(),
    )?;
    let fd = unsafe {
        libc::openat(
            parent_file.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            bail!("project registry path changed during creation");
        }
        return Err(error.into());
    }
    let content = emit_yaml(&Vec::<Value>::new())?;
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    parent_file.sync_all()?;
    Ok(())
}

pub async fn load_project_inventory(
    path: &Path,
    project_roots: &[String],
) -> Result<ProjectInventory> {
    ensure_project_registry(path)?;
    let paths = match read_registry(path).and_then(|content| parse_project_paths(&content)) {
        Ok(paths) => paths,
        Err(error) => {
            return Ok(ProjectInventory {
                projects: Vec::new(),
                diagnostics: vec![error.to_string()],
            });
        }
    };
    let roots = canonical_roots(project_roots)?;
    let mut projects = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen = HashSet::new();
    for path_value in paths {
        match inspect_project(&path_value, &roots).await {
            Ok(project) if seen.insert(project.path.clone()) => projects.push(project),
            Ok(_) => diagnostics.push(format!("duplicate project path: {path_value}")),
            Err(error) => diagnostics.push(format!("{path_value}: {error}")),
        }
    }
    Ok(ProjectInventory {
        projects,
        diagnostics,
    })
}

pub async fn find_likely_project(
    repository: &str,
    project_roots: &[String],
) -> Result<Option<ProjectInventoryEntry>> {
    let mut parts = repository.split('/');
    let (Some(_owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return Ok(None);
    };
    if name.is_empty() || repository.starts_with('/') {
        return Ok(None);
    }
    let roots = canonical_roots(project_roots)?;
    for root in &roots {
        let candidate = root.join(name).to_string_lossy().into_owned();
        if let Ok(project) = inspect_project(&candidate, &roots).await
            && project
                .github_repositories
                .iter()
                .any(|value| value.eq_ignore_ascii_case(repository))
        {
            return Ok(Some(project));
        }
    }
    Ok(None)
}

pub async fn validate_project_registry_write(
    requested_path: &Path,
    content: &str,
    registry_path: &Path,
    project_roots: &[String],
) -> Result<Vec<ProjectInventoryEntry>> {
    let requested = normalize(&expand_path(requested_path)?)?;
    let registry = normalize(&expand_path(registry_path)?)?;
    let canonical_registry = fs::canonicalize(&registry)
        .with_context(|| format!("project registry is unavailable: {}", registry.display()))?;
    if requested != registry && requested != canonical_registry {
        bail!("write access is limited to the project registry");
    }
    validate_project_registry_content(content, project_roots).await
}

pub async fn validate_project_registry_content(
    content: &str,
    project_roots: &[String],
) -> Result<Vec<ProjectInventoryEntry>> {
    if content.len() > MAX_PROJECT_REGISTRY_BYTES {
        bail!("project registry exceeds its size limit");
    }
    let paths = parse_project_paths(content)?;
    let roots = canonical_roots(project_roots)?;
    let mut projects = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let project = inspect_project(&path, &roots).await?;
        if !seen.insert(project.path.clone()) {
            bail!("duplicate project path: {path}");
        }
        projects.push(project);
    }
    Ok(projects)
}

pub async fn replace_project_registry(
    requested_path: &Path,
    content: &str,
    registry_path: &Path,
    project_roots: &[String],
) -> Result<()> {
    validate_project_registry_write(requested_path, content, registry_path, project_roots).await?;
    let expanded_registry = expand_path(registry_path)?;
    if fs::symlink_metadata(&expanded_registry)?
        .file_type()
        .is_symlink()
    {
        bail!("project registry replacement refuses a symbolic link destination");
    }
    let registry = normalize(&expanded_registry)?;
    atomic_replace_no_follow(&registry, content.as_bytes())
}

fn read_registry(path: &Path) -> Result<String> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("project registry is unavailable: {}", path.display()))?;
    let mut file = open_absolute_no_follow(&canonical, false)
        .with_context(|| format!("open project registry: {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("project registry is not a regular file");
    }
    if metadata.len() > MAX_PROJECT_REGISTRY_BYTES as u64 {
        bail!("project registry exceeds its size limit");
    }
    let mut bytes = Vec::new();
    std::io::Read::take(&mut file, (MAX_PROJECT_REGISTRY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PROJECT_REGISTRY_BYTES {
        bail!("project registry exceeds its size limit");
    }
    String::from_utf8(bytes).context("project registry is not valid UTF-8")
}

fn parse_project_paths(content: &str) -> Result<Vec<String>> {
    if content.len() > MAX_PROJECT_REGISTRY_BYTES {
        bail!("project registry exceeds its size limit");
    }
    let value = parse_yaml_value(content)
        .map_err(|error| anyhow::anyhow!("invalid project registry YAML: {error}"))?;
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("project registry must be a YAML list of paths"))?;
    if values.len() > MAX_PROJECTS {
        bail!("project registry exceeds its {MAX_PROJECTS} path limit");
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| match value.as_str() {
            Some(path) if !path.trim().is_empty() => Ok(path.to_string()),
            _ => bail!("project registry entry {} must be a path", index + 1),
        })
        .collect()
}

async fn inspect_project(path_value: &str, roots: &[PathBuf]) -> Result<ProjectInventoryEntry> {
    let requested = expand_path(path_value)?;
    let canonical =
        fs::canonicalize(&requested).map_err(|_| anyhow::anyhow!("path is unavailable"))?;
    if !is_within(&canonical, roots) {
        bail!("path is outside configured project roots");
    }
    let repository_root = git_output(&canonical, &["rev-parse", "--show-toplevel"])
        .await?
        .ok_or_else(|| anyhow::anyhow!("path is not a Git repository"))?;
    let repository_path = fs::canonicalize(repository_root)?;
    if !is_within(&repository_path, roots) {
        bail!("Git repository is outside configured project roots");
    }
    let remote_output = git_output(
        &repository_path,
        &["config", "--get-regexp", "^remote\\..*\\.url$"],
    )
    .await?
    .unwrap_or_default();
    let mut remotes = Vec::new();
    for line in remote_output.lines() {
        if let Some(value) = line.split_whitespace().nth(1)
            && !remotes.iter().any(|remote| remote == value)
        {
            remotes.push(value.to_string());
        }
    }
    let mut github_repositories = Vec::new();
    for remote in &remotes {
        if let Some(repository) = github_repository(remote)
            && !github_repositories.contains(&repository)
        {
            github_repositories.push(repository);
        }
    }
    let default_branch = git_output(
        &repository_path,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await?
    .map(|value| value.strip_prefix("origin/").unwrap_or(&value).to_string());
    Ok(ProjectInventoryEntry {
        path: repository_path,
        remotes,
        github_repositories,
        default_branch,
    })
}

async fn git_output(cwd: &Path, arguments: &[&str]) -> Result<Option<String>> {
    let git = [Path::new("/usr/bin/git"), Path::new("/bin/git")]
        .into_iter()
        .find(|path| path.is_file())
        .context("Git executable is unavailable")?;
    let environment = [
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
    ];
    let output = run_process(
        git,
        arguments,
        ProcessOptions {
            cwd,
            environment: &environment,
            stdin: None,
            output_limit: MAX_PROJECT_REGISTRY_BYTES,
            timeout: Duration::from_secs(10),
            cancellation: CancellationToken::new(),
        },
    )
    .await?;
    if output.stdout_truncated || output.stderr_truncated {
        bail!("Git metadata exceeds its size limit");
    }
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8(output.stdout)
            .context("Git metadata is not valid UTF-8")?
            .trim()
            .to_string(),
    ))
}

fn github_repository(remote: &str) -> Option<String> {
    let lower = remote.to_ascii_lowercase();
    if lower.starts_with("git@github.com:") {
        return repository_pair(&remote[15..]);
    }
    let url = Url::parse(remote).ok()?;
    if !url.host_str()?.eq_ignore_ascii_case("github.com") {
        return None;
    }
    repository_pair(url.path().trim_start_matches('/'))
}

fn repository_pair(value: &str) -> Option<String> {
    let value = value.strip_suffix(".git").unwrap_or(value);
    let mut parts = value.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(repository), None) if !owner.is_empty() && !repository.is_empty() => {
            Some(format!("{owner}/{repository}"))
        }
        _ => None,
    }
}

fn atomic_replace_no_follow(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().context("project registry has no parent")?;
    let canonical_parent = fs::canonicalize(parent)?;
    let name = path
        .file_name()
        .context("project registry has no file name")?;
    let parent_file = open_absolute_no_follow(&canonical_parent, true).with_context(|| {
        format!(
            "open project registry directory: {}",
            canonical_parent.display()
        )
    })?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(".projects.yaml.{}.{}.tmp", std::process::id(), sequence);
    let temp = CString::new(temp_name.as_bytes())?;
    let target = CString::new(name.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            parent_file.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("create project registry temporary file");
    }
    let mut temporary = unsafe { File::from_raw_fd(fd) };
    let result = (|| -> Result<()> {
        temporary.write_all(content)?;
        temporary.sync_all()?;
        let rename = unsafe {
            libc::renameat(
                parent_file.as_raw_fd(),
                temp.as_ptr(),
                parent_file.as_raw_fd(),
                target.as_ptr(),
            )
        };
        if rename != 0 {
            return Err(std::io::Error::last_os_error()).context("replace project registry");
        }
        parent_file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(parent_file.as_raw_fd(), temp.as_ptr(), 0);
        }
    }
    result
}

fn normalize(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}
