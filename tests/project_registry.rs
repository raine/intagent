use std::fs;
use std::path::Path;
use std::process::Command;

use intagent::project_registry::{
    MAX_PROJECT_REGISTRY_BYTES, MAX_PROJECTS, ensure_project_registry, find_likely_project,
    load_project_inventory, replace_project_registry, validate_project_registry_content,
    validate_project_registry_write,
};
use tempfile::TempDir;

fn initialize_repository(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    run_git(&["init", "--initial-branch=main", path.to_str().unwrap()]);
    run_git(&[
        "-C",
        path.to_str().unwrap(),
        "remote",
        "add",
        "origin",
        "git@github.com:owner/example.git",
    ]);
    run_git(&[
        "-C",
        path.to_str().unwrap(),
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
}

fn run_git(arguments: &[&str]) {
    let output = Command::new("/usr/bin/git")
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn creates_a_private_empty_registry() {
    let root = TempDir::new().unwrap();
    let registry = root.path().join("config/projects.yaml");
    ensure_project_registry(&registry).unwrap();
    assert_eq!(fs::read_to_string(&registry).unwrap(), "[]\n");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&registry).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn derives_repository_metadata_and_likely_matches() {
    let root = TempDir::new().unwrap();
    let code = root.path().join("code");
    let repository = code.join("example");
    initialize_repository(&repository);
    let registry = root.path().join("projects.yaml");
    fs::write(&registry, format!("- {}\n", repository.display())).unwrap();
    let roots = vec![code.display().to_string()];

    let inventory = load_project_inventory(&registry, &roots).await.unwrap();
    assert!(inventory.diagnostics.is_empty());
    assert_eq!(inventory.projects.len(), 1);
    assert_eq!(
        inventory.projects[0].path,
        fs::canonicalize(&repository).unwrap()
    );
    assert_eq!(
        inventory.projects[0].remotes,
        ["git@github.com:owner/example.git"]
    );
    assert_eq!(inventory.projects[0].github_repositories, ["owner/example"]);
    assert_eq!(
        inventory.projects[0].default_branch.as_deref(),
        Some("main")
    );

    let likely = find_likely_project("OWNER/EXAMPLE", &roots)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(likely.path, fs::canonicalize(repository).unwrap());
    assert!(
        find_likely_project("other/example", &roots)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        find_likely_project("invalid", &roots)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn validates_complete_registry_replacements() {
    let root = TempDir::new().unwrap();
    let code = root.path().join("code");
    let repository = code.join("example");
    initialize_repository(&repository);
    let registry = root.path().join("config/projects.yaml");
    ensure_project_registry(&registry).unwrap();
    let roots = vec![code.display().to_string()];
    let content = format!("- {}\n", repository.display());

    validate_project_registry_write(&registry, &content, &registry, &roots)
        .await
        .unwrap();
    let error = validate_project_registry_write(
        &registry.with_file_name("other.yaml"),
        &content,
        &registry,
        &roots,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("limited to the project registry"), "{error}");

    replace_project_registry(&registry, &content, &registry, &roots)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&registry).unwrap(), content);
}

#[tokio::test]
async fn rejects_invalid_bounded_and_unsafe_registry_content() {
    let root = TempDir::new().unwrap();
    let code = root.path().join("code");
    let outside = root.path().join("outside/example");
    initialize_repository(&outside);
    let roots = vec![code.display().to_string()];

    for content in [
        "projects: []\n",
        "- &path /tmp\n- *path\n",
        "- /tmp\n- /tmp\n",
    ] {
        assert!(
            validate_project_registry_content(content, &roots)
                .await
                .is_err()
        );
    }
    let error = validate_project_registry_content(&format!("- {}\n", outside.display()), &roots)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("outside configured project roots"),
        "{error}"
    );
    assert!(
        validate_project_registry_content(&"x".repeat(MAX_PROJECT_REGISTRY_BYTES + 1), &roots)
            .await
            .is_err()
    );
    let too_many = (0..=MAX_PROJECTS)
        .map(|index| format!("- /tmp/{index}\n"))
        .collect::<String>();
    assert!(
        validate_project_registry_content(&too_many, &roots)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn replacement_does_not_follow_a_swapped_registry_symlink() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let code = root.path().join("code");
    let repository = code.join("example");
    initialize_repository(&repository);
    let registry = root.path().join("config/projects.yaml");
    ensure_project_registry(&registry).unwrap();
    let secret = root.path().join("secret.txt");
    fs::write(&secret, "unchanged").unwrap();
    fs::remove_file(&registry).unwrap();
    symlink(&secret, &registry).unwrap();
    let content = format!("- {}\n", repository.display());
    let roots = vec![code.display().to_string()];

    assert!(
        replace_project_registry(&registry, &content, &registry, &roots)
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(secret).unwrap(), "unchanged");
}
