use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::agent::read_policy::open_absolute_no_follow;
use crate::config::{IntagentConfig, canonical_roots, expand_path, is_within, parse_yaml_value};

const MAX_SKILL_FILE_BYTES: usize = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub disable_model_invocation: bool,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillValidation {
    pub skill_paths: Vec<PathBuf>,
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
}

pub fn validate_skills(config: &IntagentConfig) -> Result<SkillValidation> {
    let approved_roots = canonical_roots(&config.skills.approved_roots)?;
    let mut skill_paths = Vec::new();
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();

    for configured_directory in &config.skills.directories {
        let directory = expand_path(configured_directory)?;
        let mut entries = match fs::read_dir(&directory) {
            Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
            Err(error) => {
                diagnostics.push(format!("{}: {error}", directory.display()));
                continue;
            }
        };
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if entry.file_name().as_bytes().first() == Some(&b'.') {
                continue;
            }
            let candidate = entry.path();
            match validate_skill(&candidate, &approved_roots) {
                Ok(skill) => {
                    skill_paths.push(candidate);
                    skills.push(skill);
                }
                Err(error) => diagnostics.push(format!("{}: {error}", candidate.display())),
            }
        }
    }
    skills.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(SkillValidation {
        skill_paths,
        skills,
        diagnostics,
    })
}

pub fn format_skill_catalog(skills: &[Skill]) -> String {
    let visible = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks."
            .to_string(),
        "Use the read tool to load a skill's file when the task matches its description."
            .to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands."
            .to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    for skill in visible {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.path.to_string_lossy())
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn skill_working_directory(skill_path: &Path) -> Option<&Path> {
    skill_path.parent()
}

fn validate_skill(candidate: &Path, approved_roots: &[PathBuf]) -> Result<Skill> {
    let canonical = fs::canonicalize(candidate).context("skill path is unavailable")?;
    let metadata = fs::symlink_metadata(&canonical)?;
    if !metadata.is_dir() {
        bail!("skill entry is not a directory");
    }
    if !is_within(&canonical, approved_roots) {
        bail!("canonical skill path is outside approved roots");
    }
    let skill_file = canonical.join("SKILL.md");
    let skill_metadata = fs::symlink_metadata(&skill_file)
        .map_err(|_| anyhow::anyhow!("SKILL.md is missing or is not a file"))?;
    if !skill_metadata.is_file() {
        bail!("SKILL.md is missing or is not a file");
    }
    validate_links(candidate, approved_roots)?;
    let frontmatter = parse_skill_file(&skill_file, approved_roots)
        .map_err(|error| anyhow::anyhow!("{}: {error}", skill_file.display()))?;
    Ok(Skill {
        name: frontmatter.name,
        description: frontmatter.description,
        disable_model_invocation: frontmatter.disable_model_invocation,
        path: skill_file,
    })
}

fn validate_links(root: &Path, approved_roots: &[PathBuf]) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut visited = HashSet::new();
    while let Some(path) = pending.pop() {
        let lexical = absolute(&path)?;
        if !visited.insert(lexical) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            let target = fs::canonicalize(&path)
                .with_context(|| format!("{}: broken symbolic link", path.display()))?;
            if !is_within(&target, approved_roots) {
                bail!(
                    "{}: symbolic link target is outside approved roots: {}",
                    path.display(),
                    target.display()
                );
            }
            let target_metadata = fs::symlink_metadata(&target)?;
            if target_metadata.is_dir() {
                let linked_skill = target.join("SKILL.md");
                if !fs::symlink_metadata(&linked_skill).is_ok_and(|value| value.is_file()) {
                    bail!(
                        "{}: linked skill target has no SKILL.md file",
                        path.display()
                    );
                }
                pending.push(target);
            }
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        let mut entries = fs::read_dir(&path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if entry.file_name().as_bytes().first() != Some(&b'.') {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

fn parse_skill_file(path: &Path, approved_roots: &[PathBuf]) -> Result<SkillFrontmatter> {
    let canonical = fs::canonicalize(path)?;
    if !is_within(&canonical, approved_roots) {
        bail!("canonical skill file is outside approved roots");
    }
    let file = open_absolute_no_follow(&canonical, false)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_SKILL_FILE_BYTES as u64 {
        bail!("skill file is unavailable or exceeds its size limit");
    }
    let mut bytes = Vec::new();
    file.take((MAX_SKILL_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SKILL_FILE_BYTES {
        bail!("skill file exceeds its size limit");
    }
    let content = String::from_utf8(bytes).context("skill file is not valid UTF-8")?;
    let frontmatter = extract_frontmatter(&content)?;
    let value = parse_yaml_value(frontmatter)
        .map_err(|error| anyhow::anyhow!("invalid Agent Skills frontmatter: {error}"))?;
    let parsed: SkillFrontmatter = serde_json::from_value(value)
        .map_err(|error| anyhow::anyhow!("invalid Agent Skills frontmatter: {error}"))?;
    if parsed.name.trim().is_empty() {
        bail!("invalid Agent Skills frontmatter: name must not be empty");
    }
    if parsed.description.trim().is_empty() {
        bail!("invalid Agent Skills frontmatter: description must not be empty");
    }
    Ok(parsed)
}

fn extract_frontmatter(content: &str) -> Result<&str> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or_else(|| {
            anyhow::anyhow!("invalid Agent Skills frontmatter: missing opening delimiter")
        })?;
    let Some(end) = rest.find("\n---") else {
        bail!("invalid Agent Skills frontmatter: missing closing delimiter");
    };
    let after = &rest[end + 4..];
    if !after.is_empty() && !after.starts_with('\n') && !after.starts_with("\r\n") {
        bail!("invalid Agent Skills frontmatter: malformed closing delimiter");
    }
    Ok(&rest[..end])
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
