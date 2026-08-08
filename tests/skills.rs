use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use intake::agent::skills::{format_skill_catalog, validate_skills};
use intake::config::load_config;
use tempfile::TempDir;

fn fixture_config() -> intake::config::IntakeConfig {
    load_config(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config/valid.yaml"))
        .unwrap()
}

fn write_skill(path: &Path, frontmatter: &str) {
    fs::create_dir_all(path).unwrap();
    fs::write(
        path.join("SKILL.md"),
        format!("---\n{frontmatter}\n---\nInstructions.\n"),
    )
    .unwrap();
}

fn configure(root: &TempDir, directories: Vec<PathBuf>) -> intake::config::IntakeConfig {
    let mut config = fixture_config();
    config.skills.directories = directories
        .into_iter()
        .map(|path| path.display().to_string())
        .collect();
    config.skills.approved_roots = vec![root.path().display().to_string()];
    config
}

#[test]
fn validates_linked_skills_and_stable_catalog_metadata() {
    let root = TempDir::new().unwrap();
    let wrappers = root.path().join("wrappers");
    let wrapper = wrappers.join("github-investigation");
    let reference = root.path().join("references/workmux");
    write_skill(
        &wrapper,
        "name: github-investigation\ndescription: Investigate GitHub intake.",
    );
    write_skill(
        &reference,
        "name: workmux\ndescription: Worktree reference.\ndisable-model-invocation: true",
    );
    fs::create_dir_all(wrapper.join("references")).unwrap();
    symlink(&reference, wrapper.join("references/workmux")).unwrap();
    let validation = validate_skills(&configure(&root, vec![wrappers])).unwrap();

    assert!(
        validation.diagnostics.is_empty(),
        "{:?}",
        validation.diagnostics
    );
    assert_eq!(validation.skill_paths, [wrapper]);
    assert_eq!(validation.skills[0].name, "github-investigation");
    assert!(!validation.skills[0].disable_model_invocation);
    let catalog = format_skill_catalog(&validation.skills);
    assert!(catalog.contains("github-investigation"));
    assert!(catalog.contains("Investigate GitHub intake."));
    assert!(catalog.contains(validation.skills[0].path.to_str().unwrap()));
}

#[test]
fn excludes_disabled_and_dot_skills_from_the_catalog() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("skills");
    write_skill(
        &directory.join("reference"),
        "name: reference\ndescription: Reference only.\ndisable-model-invocation: true",
    );
    write_skill(
        &directory.join(".hidden"),
        "name: hidden\ndescription: Hidden.",
    );
    let validation = validate_skills(&configure(&root, vec![directory])).unwrap();
    assert!(validation.diagnostics.is_empty());
    assert_eq!(validation.skills.len(), 1);
    assert!(format_skill_catalog(&validation.skills).is_empty());
}

#[test]
fn rejects_unsafe_links_and_linked_directories_without_skill_files() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let wrappers = root.path().join("wrappers");
    let wrapper = wrappers.join("mail");
    write_skill(&wrapper, "name: mail\ndescription: Triage mail.");
    fs::create_dir_all(wrapper.join("references")).unwrap();
    write_skill(
        &outside.path().join("private"),
        "name: private\ndescription: Private.",
    );
    symlink(
        outside.path().join("private"),
        wrapper.join("references/private"),
    )
    .unwrap();
    let validation = validate_skills(&configure(&root, vec![wrappers.clone()])).unwrap();
    assert!(validation.skill_paths.is_empty());
    assert!(
        validation
            .diagnostics
            .join("\n")
            .contains("outside approved roots")
    );

    fs::remove_file(wrapper.join("references/private")).unwrap();
    let incomplete = root.path().join("references/incomplete");
    fs::create_dir_all(&incomplete).unwrap();
    symlink(&incomplete, wrapper.join("references/incomplete")).unwrap();
    let validation = validate_skills(&configure(&root, vec![wrappers])).unwrap();
    assert!(validation.skill_paths.is_empty());
    assert!(
        validation
            .diagnostics
            .join("\n")
            .contains("has no SKILL.md")
    );
}

#[test]
fn repository_skill_examples_are_valid_templates() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/skills");
    let mut config = fixture_config();
    config.skills.directories = vec![directory.display().to_string()];
    config.skills.approved_roots = vec![directory.display().to_string()];

    let validation = validate_skills(&config).unwrap();
    assert!(validation.diagnostics.is_empty());
    assert_eq!(validation.skills.len(), 2);
    let catalog = format_skill_catalog(&validation.skills);
    assert!(catalog.contains("email-triage"));
    assert!(catalog.contains("github-investigation"));
}

#[test]
fn reports_path_qualified_malformed_frontmatter() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("skills");
    let malformed = directory.join("malformed");
    fs::create_dir_all(&malformed).unwrap();
    fs::write(
        malformed.join("SKILL.md"),
        "---\nname: malformed\ndescription: [\n---\n",
    )
    .unwrap();
    let validation = validate_skills(&configure(&root, vec![directory])).unwrap();
    let diagnostic = validation.diagnostics.join("\n");
    assert!(diagnostic.contains("SKILL.md"), "{diagnostic}");
    assert!(diagnostic.contains("frontmatter"), "{diagnostic}");
}
