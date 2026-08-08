use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::{Regex, RegexBuilder};
use tokio_util::sync::CancellationToken;
use tree_sitter::{Node, Parser};

use crate::agent::process::{ProcessFailure, ProcessOptions, run_process};
use crate::config::{CommandRule, IntakeConfig, is_within};

pub const MAX_COMMAND_STDIN_BYTES: usize = 256 * 1024;
const MAX_COMMAND_UTF16_UNITS: usize = 32_768;
const MAX_PIPELINE_STAGES: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCommand {
    pub stages: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub failure: Option<ProcessFailure>,
}

#[derive(Clone, Debug)]
pub struct CommandPolicy {
    working_roots: Vec<PathBuf>,
    path: Vec<PathBuf>,
    rules: HashMap<String, CommandRule>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    filters: Vec<Regex>,
}

impl CommandPolicy {
    pub fn new(config: &IntakeConfig, canonical_working_roots: Vec<PathBuf>) -> Result<Self> {
        let path = config
            .commands
            .path
            .iter()
            .map(|value| normalize(Path::new(value)))
            .collect::<Result<Vec<_>>>()?;
        let mut filters = vec![
            RegexBuilder::new(r"(https?://)[^/\s:@]+:[^@\s/]+@")
                .case_insensitive(true)
                .build()?,
            Regex::new(r"\b(?:sk-[a-zA-Z0-9_-]{16,}|gh[opurs]_[a-zA-Z0-9]{20,})\b")?,
            RegexBuilder::new(r"\b(?:bearer|token|password|secret)\s*[:=]\s*\S+")
                .case_insensitive(true)
                .build()?,
        ];
        for pattern in &config.commands.sensitive_patterns {
            filters.push(
                RegexBuilder::new(pattern)
                    .case_insensitive(true)
                    .build()
                    .with_context(|| format!("invalid sensitive command pattern: {pattern}"))?,
            );
        }
        Ok(Self {
            working_roots: canonical_working_roots,
            path,
            rules: config
                .commands
                .rules
                .iter()
                .cloned()
                .map(|rule| (rule.executable.clone(), rule))
                .collect(),
            timeout: Duration::from_secs(config.commands.timeout_seconds),
            max_output_bytes: config.commands.max_output_bytes,
            filters,
        })
    }

    pub fn parse_and_authorize(&self, command: &str, cwd: &Path) -> Result<ParsedCommand> {
        let utf16_len = command.encode_utf16().count();
        if utf16_len == 0 || utf16_len > MAX_COMMAND_UTF16_UNITS {
            bail!("command length is outside policy bounds");
        }
        if command.as_bytes().contains(&0) {
            bail!("NUL bytes are forbidden");
        }
        let stages = tokenize(command)?;
        let resolved_cwd = normalize(cwd)?;
        if !is_within(&resolved_cwd, &self.working_roots) {
            bail!(
                "working directory is outside approved roots: {}",
                cwd.display()
            );
        }
        validate_ast(command, &stages)?;
        for stage in &stages {
            self.authorize_stage(stage)?;
        }
        Ok(ParsedCommand { stages })
    }

    pub async fn execute(
        &self,
        command: &str,
        cwd: &Path,
        cancellation: CancellationToken,
        input: Option<&str>,
    ) -> Result<CommandResult> {
        let canonical_cwd = fs::canonicalize(cwd)
            .map_err(|_| anyhow::anyhow!("working directory is unavailable: {}", cwd.display()))?;
        let parsed = self.parse_and_authorize(command, &canonical_cwd)?;
        let input = input.map(str::as_bytes);
        if input.is_some_and(|bytes| bytes.len() > MAX_COMMAND_STDIN_BYTES) {
            bail!("command stdin exceeds policy bounds");
        }

        let executables = parsed
            .stages
            .iter()
            .map(|stage| self.resolve_executable(&stage[0]))
            .collect::<Result<Vec<_>>>()?;
        if cancellation.is_cancelled() {
            bail!("command cancelled");
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let environment = vec![
            (
                "PATH".to_string(),
                self.path
                    .iter()
                    .map(|path| path.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(":"),
            ),
            ("HOME".to_string(), home),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("LC_ALL".to_string(), "C.UTF-8".to_string()),
            ("NO_COLOR".to_string(), "1".to_string()),
            ("TERM".to_string(), "dumb".to_string()),
        ];
        let mut stdin = input.map(ToOwned::to_owned);
        let mut stderr = String::new();
        let mut truncated = false;
        let mut exit_code = 0;
        let mut failure = None;

        for (stage, executable) in parsed.stages.iter().zip(executables) {
            let output = run_process(
                &executable,
                &stage[1..],
                ProcessOptions {
                    cwd: &canonical_cwd,
                    environment: &environment,
                    stdin,
                    output_limit: self.max_output_bytes,
                    timeout: self.timeout,
                    cancellation: cancellation.child_token(),
                },
            )
            .await?;
            exit_code = output.status.code().unwrap_or(-1);
            failure = output.failure;
            stdin = Some(output.stdout);
            stderr.push_str(&String::from_utf8_lossy(&output.stderr));
            if output.stderr_truncated {
                stderr.push_str("\n[stderr truncated]");
            }
            truncated |= output.stdout_truncated || output.stderr_truncated;
            if failure.is_some() || !output.status.success() {
                break;
            }
        }
        Ok(CommandResult {
            exit_code,
            stdout: self.filter(&String::from_utf8_lossy(
                stdin.as_deref().unwrap_or_default(),
            )),
            stderr: self.filter(&stderr),
            truncated,
            failure,
        })
    }

    pub fn filter(&self, value: &str) -> String {
        self.filters
            .iter()
            .fold(value.to_string(), |filtered, pattern| {
                pattern.replace_all(&filtered, "[REDACTED]").into_owned()
            })
    }

    fn authorize_stage(&self, argv: &[String]) -> Result<()> {
        let executable = argv.first().map(String::as_str).unwrap_or_default();
        if !self.rules.contains_key(executable) {
            bail!("executable is not allowed: {executable}");
        }
        Ok(())
    }

    fn resolve_executable(&self, name: &str) -> Result<PathBuf> {
        for directory in &self.path {
            let candidate = directory.join(name);
            let Ok(metadata) = fs::symlink_metadata(&candidate) else {
                continue;
            };
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.permissions().mode() & 0o111 == 0
            {
                continue;
            }
            let canonical = fs::canonicalize(&candidate)?;
            let canonical_directory =
                fs::canonicalize(directory).unwrap_or_else(|_| directory.clone());
            if is_within(&canonical, &[canonical_directory]) {
                return Ok(canonical);
            }
        }
        bail!("allowed executable is unavailable on the fixed PATH: {name}")
    }
}

fn tokenize(source: &str) -> Result<Vec<Vec<String>>> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let mut stages = vec![Vec::new()];
    let mut word = String::new();
    let mut word_started = false;
    let mut quote = Quote::None;
    let mut escaped = false;
    let mut word_start = true;

    for character in source.chars() {
        if quote == Quote::None && matches!(character, '\r' | '\n') {
            bail!("newlines outside quoted arguments are forbidden");
        }
        if escaped {
            if quote == Quote::Double && !matches!(character, '$' | '`' | '"' | '\\' | '\r' | '\n')
            {
                word.push('\\');
            }
            if !matches!(character, '\r' | '\n') {
                word.push(character);
            }
            word_start = false;
            escaped = false;
            continue;
        }
        match quote {
            Quote::Single => {
                if character == '\'' {
                    quote = Quote::None;
                } else {
                    word.push(character);
                }
                word_started = true;
                continue;
            }
            Quote::Double => {
                match character {
                    '\\' => escaped = true,
                    '"' => quote = Quote::None,
                    '$' | '`' => bail!("shell expansions are forbidden"),
                    _ => word.push(character),
                }
                word_started = true;
                continue;
            }
            Quote::None => {}
        }
        match character {
            '\\' => {
                escaped = true;
                word_started = true;
                word_start = false;
            }
            '\'' => {
                quote = Quote::Single;
                word_started = true;
                word_start = false;
            }
            '"' => {
                quote = Quote::Double;
                word_started = true;
                word_start = false;
            }
            '|' => {
                finish_word(&mut stages, &mut word, &mut word_started);
                if stages.last().is_none_or(Vec::is_empty) {
                    bail!("pipeline contains an empty stage");
                }
                if stages.len() >= MAX_PIPELINE_STAGES {
                    bail!("pipeline stage count is outside policy bounds");
                }
                stages.push(Vec::new());
                word_start = true;
            }
            value if value.is_whitespace() => {
                finish_word(&mut stages, &mut word, &mut word_started);
                word_start = true;
            }
            '#' if word_start => bail!("shell comments are forbidden"),
            ';' | '&' | '<' | '>' | '`' | '$' | '(' | ')' | '{' | '}' => {
                bail!("shell operator is forbidden: {character}")
            }
            '?' | '*' | '[' => bail!("unquoted glob syntax is forbidden"),
            _ => {
                word.push(character);
                word_started = true;
                word_start = false;
            }
        }
    }
    if escaped || quote != Quote::None {
        bail!("command syntax is invalid: incomplete quote or escape");
    }
    finish_word(&mut stages, &mut word, &mut word_started);
    if stages.last().is_none_or(Vec::is_empty) {
        bail!("pipeline contains an empty stage");
    }
    if stages.is_empty() || stages.len() > MAX_PIPELINE_STAGES {
        bail!("pipeline stage count is outside policy bounds");
    }
    Ok(stages)
}

fn finish_word(stages: &mut [Vec<String>], word: &mut String, started: &mut bool) {
    if *started {
        stages
            .last_mut()
            .expect("tokenizer always has one stage")
            .push(std::mem::take(word));
        *started = false;
    }
}

fn validate_ast(source: &str, expected_stages: &[Vec<String>]) -> Result<()> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .context("load pinned Bash grammar")?;
    let tree = parser
        .parse(source, None)
        .context("Bash parser did not produce a syntax tree")?;
    let root = tree.root_node();
    if root.kind() != "program" || root.has_error() || root.is_error() || root.is_missing() {
        bail!("command syntax is invalid or recovered by the parser");
    }
    let named = named_children(root);
    if named.len() != 1 {
        bail!("only one command or one pipeline is permitted");
    }
    let top = named[0];
    let commands = match top.kind() {
        "command" => vec![top],
        "pipeline" => {
            ensure_anonymous_children(top, &["|"])?;
            let children = named_children(top);
            if children.iter().any(|node| node.kind() != "command") {
                bail!("pipeline contains a forbidden construct");
            }
            children
        }
        kind => bail!("shell construct is forbidden: {kind}"),
    };
    if commands.len() != expected_stages.len()
        || commands.is_empty()
        || commands.len() > MAX_PIPELINE_STAGES
    {
        bail!("parser and command tokenizer disagree on pipeline structure");
    }
    for (command, expected) in commands.into_iter().zip(expected_stages) {
        validate_command_node(command, expected.len())?;
    }
    Ok(())
}

fn validate_command_node(node: Node<'_>, expected_arguments: usize) -> Result<()> {
    if node.kind() != "command" || node.has_error() || node.is_error() || node.is_missing() {
        bail!("pipeline contains a forbidden construct");
    }
    let children = named_children(node);
    if children.is_empty() || children[0].kind() != "command_name" {
        bail!("assignments and empty commands are forbidden");
    }
    if children.len() != expected_arguments {
        bail!("parser and command tokenizer disagree on argument structure");
    }
    for child in children {
        validate_literal_node(child)?;
    }
    Ok(())
}

fn validate_literal_node(node: Node<'_>) -> Result<()> {
    if node.has_error() || node.is_error() || node.is_missing() {
        bail!("command syntax is invalid or recovered by the parser");
    }
    const ALLOWED: &[&str] = &[
        "command_name",
        "word",
        "number",
        "string",
        "string_content",
        "raw_string",
        "concatenation",
    ];
    if !ALLOWED.contains(&node.kind()) {
        bail!("shell construct is forbidden: {}", node.kind());
    }
    let anonymous = match node.kind() {
        "string" => &["\""][..],
        _ => &[][..],
    };
    ensure_anonymous_children(node, anonymous)?;
    for child in named_children(node) {
        validate_literal_node(child)?;
    }
    Ok(())
}

fn ensure_anonymous_children(node: Node<'_>, allowed: &[&str]) -> Result<()> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor).filter(|child| !child.is_named()) {
        if child.is_error() || child.is_missing() || !allowed.contains(&child.kind()) {
            bail!("unexpected parser node: {}", child.kind());
        }
    }
    Ok(())
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
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

#[cfg(test)]
mod tests {
    use super::tokenize;

    #[test]
    fn tokenizes_literal_quotes_and_escapes() {
        assert_eq!(
            tokenize("aven search \\\"literal\\\" 'login issue'").unwrap(),
            vec![vec!["aven", "search", "\"literal\"", "login issue"]]
        );
    }
}
