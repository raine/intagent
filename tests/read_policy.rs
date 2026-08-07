use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use intake::agent::read_policy::{
    MAX_READ_FILE_BYTES, MAX_READ_LINES, MAX_READ_PATH_BYTES, ReadInput, ReadPolicy,
};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    project: PathBuf,
    skills: PathBuf,
    outside: TempDir,
    policy: ReadPolicy,
}

fn fixture(max_output_bytes: usize) -> Fixture {
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    let skills = root.path().join("skills");
    fs::create_dir(&project).unwrap();
    fs::create_dir(&skills).unwrap();
    let policy = ReadPolicy::new(vec![project.clone(), skills.clone()], max_output_bytes).unwrap();
    Fixture {
        _root: root,
        project,
        skills,
        outside: TempDir::new().unwrap(),
        policy,
    }
}

fn input(path: impl Into<String>) -> ReadInput {
    ReadInput {
        path: path.into(),
        offset: None,
        limit: None,
    }
}

#[test]
fn reads_approved_files_with_line_numbers_and_ranges() {
    let fixture = fixture(1024);
    fs::write(fixture.project.join("notes.txt"), "alpha\nbeta").unwrap();
    fs::write(fixture.skills.join("SKILL.md"), "rules\napply").unwrap();
    let result = fixture
        .policy
        .read(&input("notes.txt"), &fixture.project)
        .unwrap();
    assert_eq!(result.text, "1\talpha\n2\tbeta");
    assert_eq!(
        result.path,
        fs::canonicalize(fixture.project.join("notes.txt")).unwrap()
    );

    fs::write(fixture.project.join("lines.txt"), "one\ntwo\nthree\nfour").unwrap();
    let ranged = fixture
        .policy
        .read(
            &ReadInput {
                path: "lines.txt".into(),
                offset: Some(2),
                limit: Some(2),
            },
            &fixture.project,
        )
        .unwrap();
    assert!(ranged.text.contains("2\ttwo\n3\tthree"));
    assert!(ranged.text.contains("Showing lines 2-3 of 4"));
    assert!(ranged.text.contains("offset=4"));
    assert!(ranged.truncated);
    assert_eq!(ranged.end_line, 3);
}

#[test]
fn enforces_path_file_output_utf8_and_range_bounds() {
    let fixture = fixture(256);
    fs::write(
        fixture.project.join("large.txt"),
        vec![b'x'; MAX_READ_FILE_BYTES + 1],
    )
    .unwrap();
    assert!(
        fixture
            .policy
            .read(&input("large.txt"), &fixture.project)
            .is_err()
    );
    assert!(
        fixture
            .policy
            .read(
                &input("x".repeat(MAX_READ_PATH_BYTES + 1)),
                &fixture.project
            )
            .is_err()
    );
    assert!(
        fixture
            .policy
            .read(
                &ReadInput {
                    path: "missing".into(),
                    offset: None,
                    limit: Some(MAX_READ_LINES + 1),
                },
                &fixture.project,
            )
            .is_err()
    );
    fs::write(fixture.project.join("invalid.txt"), [0xff, 0xfe]).unwrap();
    assert!(
        fixture
            .policy
            .read(&input("invalid.txt"), &fixture.project)
            .is_err()
    );

    fs::write(fixture.project.join("output.txt"), "é".repeat(2000)).unwrap();
    let result = fixture
        .policy
        .read(&input("output.txt"), &fixture.project)
        .unwrap();
    assert!(result.text.len() <= 256);
    assert!(result.text.contains("Output truncated"));
    assert!(result.truncated);
    assert!(std::str::from_utf8(result.text.as_bytes()).is_ok());
}

#[test]
fn accepts_only_canonical_symlink_targets_beneath_roots() {
    let fixture = fixture(1024);
    let target = fixture.skills.join("reference.md");
    let link = fixture.project.join("reference.md");
    fs::write(&target, "approved").unwrap();
    symlink(&target, &link).unwrap();
    let result = fixture
        .policy
        .read(&input(link.display().to_string()), &fixture.project)
        .unwrap();
    assert_eq!(result.path, fs::canonicalize(target).unwrap());
    assert_eq!(result.text, "1\tapproved");

    let secret = fixture.outside.path().join("secret.txt");
    fs::write(&secret, "sensitive").unwrap();
    symlink(&secret, fixture.project.join("escape.txt")).unwrap();
    let error = fixture
        .policy
        .read(&input("escape.txt"), &fixture.project)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("canonical file path is outside approved roots"),
        "{error}"
    );
    assert!(
        fixture
            .policy
            .read(&input(secret.display().to_string()), &fixture.project)
            .is_err()
    );
    assert!(fixture.policy.read(&input("."), &fixture.project).is_err());
}

#[test]
fn supports_configured_roots_that_are_symlinks() {
    let fixture = fixture(1024);
    let configured = fixture.outside.path().join("configured-skills");
    symlink(&fixture.skills, &configured).unwrap();
    fs::write(fixture.skills.join("private.md"), "private skill").unwrap();
    let policy = ReadPolicy::new(vec![configured.clone()], 1024).unwrap();
    let result = policy
        .read(
            &input(configured.join("private.md").display().to_string()),
            &fixture.project,
        )
        .unwrap();
    assert_eq!(result.text, "1\tprivate skill");
}

#[test]
fn symlink_races_never_return_out_of_root_content() {
    let fixture = fixture(1024);
    let approved = fixture.project.join("approved.txt");
    let secret = fixture.outside.path().join("secret.txt");
    let link = fixture.project.join("raced.txt");
    fs::write(&approved, "approved").unwrap();
    fs::write(&secret, "secret-marker").unwrap();
    symlink(&approved, &link).unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    let link_for_thread = link.clone();
    let approved_for_thread = approved.clone();
    let secret_for_thread = secret.clone();
    let toggler = std::thread::spawn(move || {
        while flag.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&link_for_thread);
            let _ = symlink(&secret_for_thread, &link_for_thread);
            let _ = fs::remove_file(&link_for_thread);
            let _ = symlink(&approved_for_thread, &link_for_thread);
        }
    });
    for _ in 0..1000 {
        if let Ok(result) = fixture
            .policy
            .read(&input(link.display().to_string()), &fixture.project)
        {
            assert!(!result.text.contains("secret-marker"));
        }
    }
    running.store(false, Ordering::Relaxed);
    toggler.join().unwrap();
}
