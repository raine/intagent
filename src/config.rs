use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use serde_saphyr::options::{DuplicateKeyPolicy, MergeKeyPolicy};
use thiserror::Error;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Cannot read configuration at {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Invalid YAML in {path}: {message}")]
    Yaml { path: PathBuf, message: String },
    #[error("Invalid configuration at {path}: {message}")]
    Validation { path: PathBuf, message: String },
    #[error("Cannot initialize private configuration: {0}")]
    Initialize(#[from] std::io::Error),
    #[error("Cannot emit YAML: {0}")]
    Emit(String),
    #[error("HOME is not set")]
    MissingHome,
    #[error("Cannot locate the intake executable")]
    MissingExecutableDirectory,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeConfig {
    pub version: u8,
    #[serde(default = "default_project_roots")]
    pub project_roots: Vec<String>,
    #[serde(default)]
    pub state: StateConfig,
    pub skills: SkillsConfig,
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub triage: TriageConfig,
    pub commands: CommandsConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct StateConfig {
    pub database: String,
    pub logs: String,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            database: "~/.local/state/intake/intake.sqlite".into(),
            logs: "~/.local/state/intake/logs".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsConfig {
    pub directories: Vec<String>,
    pub approved_roots: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_source_interval")]
    pub interval_seconds: u64,
    #[serde(default = "default_source_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_item_limit")]
    pub item_limit: usize,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub options: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    #[default]
    Max,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriageConfig {
    pub model: String,
    pub thinking_level: ThinkingLevel,
    pub max_turns: usize,
    pub timeout_minutes: u64,
    pub max_attempts: usize,
    pub retry_base_seconds: u64,
    pub compaction_trigger_tokens: u64,
    pub compaction_keep_recent_messages: usize,
}

impl Default for TriageConfig {
    fn default() -> Self {
        Self {
            model: "gpt-5.6-luna".into(),
            thinking_level: ThinkingLevel::Max,
            max_turns: 50,
            timeout_minutes: 30,
            max_attempts: 3,
            retry_base_seconds: 60,
            compaction_trigger_tokens: 100_000,
            compaction_keep_recent_messages: 12,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandsConfig {
    pub path: Vec<String>,
    #[serde(default = "default_command_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    #[serde(default)]
    pub sensitive_patterns: Vec<String>,
    pub rules: Vec<CommandRule>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRule {
    pub executable: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitializeResult {
    pub config_path: PathBuf,
    pub created: Vec<PathBuf>,
}

impl IntakeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("version: expected 1".into());
        }
        require_nonempty("project_roots", &self.project_roots)?;
        require_nonempty("skills.directories", &self.skills.directories)?;
        require_nonempty("skills.approved_roots", &self.skills.approved_roots)?;
        require_nonempty("commands.path", &self.commands.path)?;
        require_nonempty("commands.rules", &self.commands.rules)?;

        let mut source_names = HashSet::new();
        for (index, source) in self.sources.iter().enumerate() {
            if !valid_source_name(&source.name) {
                return Err(format!(
                    "sources.{index}.name: must match ^[a-z0-9][a-z0-9-]*$"
                ));
            }
            if !source_names.insert(&source.name) {
                return Err(format!("duplicate source name: {}", source.name));
            }
            if source.command.is_empty() {
                return Err(format!("sources.{index}.command: must not be empty"));
            }
            check_range(
                &format!("sources.{index}.interval_seconds"),
                source.interval_seconds,
                10,
                MAX_SAFE_INTEGER,
            )?;
            check_range(
                &format!("sources.{index}.timeout_seconds"),
                source.timeout_seconds,
                1,
                300,
            )?;
            check_range(
                &format!("sources.{index}.item_limit"),
                source.item_limit as u64,
                1,
                1000,
            )?;
            for (environment_index, name) in source.environment.iter().enumerate() {
                if !valid_environment_name(name) {
                    return Err(format!(
                        "sources.{index}.environment.{environment_index}: invalid environment name"
                    ));
                }
            }
            for key in source.options.keys() {
                if !valid_configuration_key(key) {
                    return Err(format!("sources.{index}.options.{key}: invalid option key"));
                }
            }
        }

        if self.triage.model.is_empty() {
            return Err("triage.model: must not be empty".into());
        }
        check_range("triage.max_turns", self.triage.max_turns as u64, 1, 50)?;
        check_range("triage.timeout_minutes", self.triage.timeout_minutes, 1, 30)?;
        check_range("triage.max_attempts", self.triage.max_attempts as u64, 1, 3)?;
        check_range(
            "triage.retry_base_seconds",
            self.triage.retry_base_seconds,
            1,
            3600,
        )?;
        check_range(
            "triage.compaction_trigger_tokens",
            self.triage.compaction_trigger_tokens,
            1,
            MAX_SAFE_INTEGER,
        )?;
        check_range(
            "triage.compaction_keep_recent_messages",
            self.triage.compaction_keep_recent_messages as u64,
            1,
            MAX_SAFE_INTEGER,
        )?;
        check_range(
            "commands.timeout_seconds",
            self.commands.timeout_seconds,
            1,
            300,
        )?;
        check_range(
            "commands.max_output_bytes",
            self.commands.max_output_bytes as u64,
            1024,
            1_000_000,
        )?;

        let mut executables = HashSet::new();
        for (index, rule) in self.commands.rules.iter().enumerate() {
            if !valid_executable_name(&rule.executable) {
                return Err(format!(
                    "commands.rules.{index}.executable: invalid executable name"
                ));
            }
            if !executables.insert(&rule.executable) {
                return Err(format!("duplicate command rule: {}", rule.executable));
            }
        }
        Ok(())
    }
}

pub fn load_config(path: impl AsRef<Path>) -> Result<IntakeConfig, ConfigError> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_config(&raw).map_err(|error| match error {
        ParseConfigError::Yaml(message) => ConfigError::Yaml {
            path: path.to_path_buf(),
            message,
        },
        ParseConfigError::Validation(message) => ConfigError::Validation {
            path: path.to_path_buf(),
            message,
        },
    })
}

#[derive(Debug)]
enum ParseConfigError {
    Yaml(String),
    Validation(String),
}

pub fn parse_yaml_value(raw: &str) -> Result<Value, String> {
    let options = serde_saphyr::options! {
        duplicate_keys: DuplicateKeyPolicy::Error,
        merge_keys: MergeKeyPolicy::Error,
        strict_booleans: true,
        alias_limits: serde_saphyr::alias_limits! {
            max_alias_expansions_per_anchor: 0,
        },
    };
    serde_saphyr::from_str_with_options(raw, options).map_err(|error| error.to_string())
}

fn parse_config(raw: &str) -> Result<IntakeConfig, ParseConfigError> {
    let value = parse_yaml_value(raw).map_err(ParseConfigError::Yaml)?;
    let config: IntakeConfig = serde_json::from_value(value)
        .map_err(|error| ParseConfigError::Validation(error.to_string()))?;
    config.validate().map_err(ParseConfigError::Validation)?;
    Ok(config)
}

pub fn emit_yaml<T: Serialize>(value: &T) -> Result<String, ConfigError> {
    serde_saphyr::to_string(value).map_err(|error| ConfigError::Emit(error.to_string()))
}

pub fn config_directory() -> Result<PathBuf, ConfigError> {
    config_directory_with_env(&env_map())
}

pub fn config_directory_with_env(
    environment: &HashMap<String, String>,
) -> Result<PathBuf, ConfigError> {
    application_directory(environment, "XDG_CONFIG_HOME", &[".config", "intake"])
}

pub fn project_registry_path() -> Result<PathBuf, ConfigError> {
    Ok(config_directory()?.join("projects.yaml"))
}

pub fn state_directory() -> Result<PathBuf, ConfigError> {
    state_directory_with_env(&env_map())
}

pub fn state_directory_with_env(
    environment: &HashMap<String, String>,
) -> Result<PathBuf, ConfigError> {
    application_directory(
        environment,
        "XDG_STATE_HOME",
        &[".local", "state", "intake"],
    )
}

pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    Ok(config_directory()?.join("config.yaml"))
}

pub fn expand_path(path: impl AsRef<Path>) -> Result<PathBuf, ConfigError> {
    expand_path_with_env(path, &env_map())
}

pub fn expand_path_with_env(
    path: impl AsRef<Path>,
    environment: &HashMap<String, String>,
) -> Result<PathBuf, ConfigError> {
    let path = path.as_ref();
    let expanded = if path == Path::new("~") {
        home_directory(environment)?
    } else if let Ok(relative) = path.strip_prefix("~/") {
        home_directory(environment)?.join(relative)
    } else {
        path.to_path_buf()
    };
    absolute_path(&expanded).map_err(ConfigError::Initialize)
}

pub fn canonical_roots(paths: &[String]) -> Result<Vec<PathBuf>, ConfigError> {
    paths
        .iter()
        .map(|path| {
            let expanded = expand_path(path)?;
            Ok(fs::canonicalize(&expanded).unwrap_or(expanded))
        })
        .collect()
}

pub fn is_within(path: impl AsRef<Path>, roots: &[PathBuf]) -> bool {
    let Ok(path) = absolute_path(path.as_ref()) else {
        return false;
    };
    roots
        .iter()
        .any(|root| absolute_path(root).is_ok_and(|root| path == root || path.starts_with(root)))
}

pub fn initialize_private_config(
    path: impl AsRef<Path>,
    environment: &HashMap<String, String>,
) -> Result<InitializeResult, ConfigError> {
    let path = absolute_path(path.as_ref()).map_err(ConfigError::Initialize)?;
    let directory = path
        .parent()
        .ok_or(ConfigError::MissingExecutableDirectory)?;
    let skills_directory = directory.join("skills");
    let state = state_directory_with_env(environment)?;
    create_private_directory(directory)?;
    create_private_directory(&skills_directory)?;
    create_private_directory(&state)?;

    let home = home_directory(environment)?;
    let config = IntakeConfig {
        version: 1,
        project_roots: default_project_roots(),
        state: StateConfig {
            database: state.join("intake.sqlite").to_string_lossy().into_owned(),
            logs: state.join("logs").to_string_lossy().into_owned(),
        },
        skills: SkillsConfig {
            directories: vec![skills_directory.to_string_lossy().into_owned()],
            approved_roots: vec![
                skills_directory.to_string_lossy().into_owned(),
                home.join(".claude/skills").to_string_lossy().into_owned(),
            ],
        },
        sources: Vec::new(),
        triage: TriageConfig::default(),
        commands: CommandsConfig {
            path: vec![
                "/opt/homebrew/bin".into(),
                "/usr/local/bin".into(),
                "/usr/bin".into(),
                "/bin".into(),
            ],
            timeout_seconds: default_command_timeout(),
            max_output_bytes: default_max_output_bytes(),
            sensitive_patterns: Vec::new(),
            rules: default_command_rules(),
        },
    };

    let mut created = Vec::new();
    if create_private_file(&path, emit_yaml(&config)?.as_bytes())? {
        created.push(path.clone());
    }
    let projects = directory.join("projects.yaml");
    if create_private_file(&projects, emit_yaml(&Vec::<Value>::new())?.as_bytes())? {
        created.push(projects);
    }
    Ok(InitializeResult {
        config_path: path,
        created,
    })
}

pub fn default_command_rules() -> Vec<CommandRule> {
    ["git", "rg", "fd"]
        .into_iter()
        .map(|executable| CommandRule {
            executable: executable.into(),
        })
        .collect()
}

fn env_map() -> HashMap<String, String> {
    env::vars().collect()
}

fn application_directory(
    environment: &HashMap<String, String>,
    xdg_name: &str,
    home_components: &[&str],
) -> Result<PathBuf, ConfigError> {
    let base = if let Some(value) = environment.get(xdg_name) {
        PathBuf::from(value).join("intake")
    } else {
        let mut path = home_directory(environment)?;
        for component in home_components {
            path.push(component);
        }
        path
    };
    absolute_path(&base).map_err(ConfigError::Initialize)
}

fn home_directory(environment: &HashMap<String, String>) -> Result<PathBuf, ConfigError> {
    environment
        .get("HOME")
        .map(PathBuf::from)
        .ok_or(ConfigError::MissingHome)
}

fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
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

fn create_private_directory(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn create_private_file(path: &Path, contents: &[u8]) -> Result<bool, std::io::Error> {
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    file.write_all(contents)?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(true)
}

fn require_nonempty<T>(name: &str, values: &[T]) -> Result<(), String> {
    if values.is_empty() {
        Err(format!("{name}: must contain at least one item"))
    } else {
        Ok(())
    }
}

fn check_range(name: &str, value: u64, minimum: u64, maximum: u64) -> Result<(), String> {
    if (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name}: must be between {minimum} and {maximum}"))
    }
}

fn valid_source_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn valid_environment_name(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_configuration_key(value: &str) -> bool {
    let mut segments = value.split('_');
    segments.all(|segment| {
        segment
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn valid_executable_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
}

fn default_project_roots() -> Vec<String> {
    vec!["~/code".into()]
}

const fn default_source_interval() -> u64 {
    60
}

const fn default_source_timeout() -> u64 {
    60
}

const fn default_item_limit() -> usize {
    100
}

const fn default_command_timeout() -> u64 {
    60
}

const fn default_max_output_bytes() -> usize {
    65_536
}
