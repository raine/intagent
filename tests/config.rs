use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use intagent::config::{
    canonical_roots, emit_yaml, expand_path_with_env, initialize_private_config, is_within,
    load_config, parse_yaml_value,
};
use serde_json::json;
use tempfile::TempDir;

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

fn environment(root: &Path) -> HashMap<String, String> {
    HashMap::from([
        ("HOME".into(), root.join("home").display().to_string()),
        (
            "XDG_STATE_HOME".into(),
            root.join("state").display().to_string(),
        ),
    ])
}

fn initialized_config() -> (TempDir, PathBuf, HashMap<String, String>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("private/config.yaml");
    let environment = environment(root.path());
    initialize_private_config(&path, &environment).unwrap();
    (root, path, environment)
}

#[test]
fn loads_version_one_fixture_with_defaults() {
    let config = load_config(fixture("config/valid.yaml")).unwrap();
    assert_eq!(config.version, 1);
    assert_eq!(config.sources[0].name, "fastmail");
    assert_eq!(config.sources[1].item_limit, 100);
    assert_eq!(config.state.logs, "~/.local/state/intagent/logs");
    assert_eq!(config.triage.compaction_trigger_tokens, 100_000);
    assert_eq!(config.triage.compaction_keep_recent_messages, 12);
}

#[test]
fn matches_shared_zod_configuration_corpus() {
    let corpus: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture("config/differential.json")).unwrap()).unwrap();
    let (_root, path, _) = initialized_config();
    let original = fs::read_to_string(&path).unwrap();
    for test_case in corpus["cases"].as_array().unwrap() {
        let yaml = original.replace("sources: []", test_case["sourceYaml"].as_str().unwrap());
        fs::write(&path, yaml).unwrap();
        let result = load_config(&path);
        assert_eq!(
            result.is_ok(),
            test_case["accepted"].as_bool().unwrap(),
            "{test_case}"
        );
        if let Some(expected) = test_case.get("optionValue") {
            let config = result.unwrap();
            assert_eq!(config.sources[0].options.values().next().unwrap(), expected);
        }
    }
}

#[test]
fn rejects_duplicate_keys_aliases_and_merge_keys() {
    let duplicate = load_config(fixture("config/invalid-duplicate.yaml"))
        .unwrap_err()
        .to_string();
    assert!(duplicate.contains("duplicate"), "{duplicate}");

    let semantic_duplicate = parse_yaml_value("\"version\": 1\nversion: 1\n").unwrap_err();
    assert!(
        semantic_duplicate.contains("duplicate"),
        "{semantic_duplicate}"
    );

    let (root, path, _) = initialized_config();
    fs::write(
        &path,
        "version: 1\nproject_roots: &roots [~/code]\nstate: {}\nskills:\n  directories: *roots\n  approved_roots: [~/code]\nsources: []\ncommands:\n  path: [/bin]\n  rules: [{ executable: git }]\n",
    )
    .unwrap();
    let alias = load_config(&path).unwrap_err().to_string();
    assert!(
        alias.contains("alias") || alias.contains("Alias"),
        "{alias}"
    );

    fs::write(
        &path,
        "version: 1\nproject_roots: [~/code]\nskills:\n  directories: [./skills]\n  approved_roots: &roots [./skills]\n  <<: { approved_roots: *roots }\nsources: []\ncommands:\n  path: [/bin]\n  rules: [{ executable: git }]\n",
    )
    .unwrap();
    let merge = load_config(&path).unwrap_err().to_string();
    assert!(
        merge.contains("merge") || merge.contains("alias"),
        "{merge}"
    );
    drop(root);
}

#[test]
fn follows_yaml_1_2_scalar_semantics_and_round_trips_emission() {
    let value = parse_yaml_value("no: no\noff: off\nyes: yes\ntrue_value: true\n").unwrap();
    assert_eq!(
        value,
        json!({ "no": "no", "off": "off", "yes": "yes", "true_value": true })
    );
    let emitted = emit_yaml(&value).unwrap();
    assert_eq!(parse_yaml_value(&emitted).unwrap(), value);

    let config = load_config(fixture("config/valid.yaml")).unwrap();
    let emitted = emit_yaml(&config).unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    fs::write(&path, emitted).unwrap();
    assert_eq!(load_config(path).unwrap(), config);

    let registry = json!(["/tmp/one", "/tmp/no", "/tmp/off"]);
    let emitted = emit_yaml(&registry).unwrap();
    assert_eq!(parse_yaml_value(&emitted).unwrap(), registry);
}

#[test]
fn rejects_unknown_keys_at_every_configuration_object_level() {
    let (_root, path, _) = initialized_config();
    let original = fs::read_to_string(&path).unwrap();
    for (needle, replacement, unknown) in [
        ("version: 1", "version: 1\nunknown: true", "unknown"),
        ("state:", "state:\n  unknown: true", "unknown"),
        ("skills:", "skills:\n  unknown: true", "unknown"),
        ("triage:", "triage:\n  unknown: true", "unknown"),
        ("commands:", "commands:\n  unknown: true", "unknown"),
    ] {
        fs::write(&path, original.replacen(needle, replacement, 1)).unwrap();
        let error = load_config(&path).unwrap_err().to_string();
        assert!(error.contains(unknown), "{error}");
    }

    let source = original.replace(
        "sources: []",
        "sources:\n  - name: fastmail\n    command: intagent-fastmail-source\n    unknown: true",
    );
    fs::write(&path, source).unwrap();
    assert!(
        load_config(&path)
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );

    let rule = original.replace(
        "- executable: git",
        "- executable: git\n      unknown: true",
    );
    fs::write(&path, rule).unwrap();
    assert!(
        load_config(&path)
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );
}

#[test]
fn validates_names_uniqueness_lists_and_numeric_bounds() {
    let (_root, path, _) = initialized_config();
    let original = fs::read_to_string(&path).unwrap();
    let cases = [
        (
            "sources: []".to_string(),
            "sources:\n  - name: Bad\n    command: source".to_string(),
            "name",
        ),
        (
            "sources: []".to_string(),
            "sources:\n  - name: same\n    command: source\n  - name: same\n    command: source"
                .to_string(),
            "duplicate source",
        ),
        (
            "sources: []".to_string(),
            "sources:\n  - name: source\n    command: source\n    environment: [lowercase]"
                .to_string(),
            "environment",
        ),
        (
            "sources: []".to_string(),
            "sources:\n  - name: source\n    command: source\n    options: { camelCase: true }"
                .to_string(),
            "camelCase",
        ),
        (
            "sources: []".to_string(),
            "sources:\n  - name: source\n    command: source\n    interval_seconds: 9".to_string(),
            "interval_seconds",
        ),
        (
            "max_turns: 50".to_string(),
            "max_turns: 51".to_string(),
            "max_turns",
        ),
        (
            "max_output_bytes: 65536".to_string(),
            "max_output_bytes: 1000001".to_string(),
            "max_output_bytes",
        ),
        (
            "- executable: rg".to_string(),
            "- executable: git".to_string(),
            "duplicate command",
        ),
        (
            "- executable: git".to_string(),
            "- executable: bad/name".to_string(),
            "executable",
        ),
    ];
    for (needle, replacement, expected) in cases {
        fs::write(&path, original.replacen(&needle, &replacement, 1)).unwrap();
        let error = load_config(&path).unwrap_err().to_string();
        assert!(
            error.contains(expected),
            "expected {expected:?} in {error:?}"
        );
    }
}

#[test]
fn initializes_private_files_and_directories_idempotently() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("private/config.yaml");
    let environment = environment(root.path());
    let result = initialize_private_config(&path, &environment).unwrap();
    assert_eq!(
        result.created,
        vec![path.clone(), root.path().join("private/projects.yaml")]
    );
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(root.path().join("private/projects.yaml"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(root.path().join("private"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.path().join("private/skills"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.path().join("state/intagent"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert!(
        initialize_private_config(&path, &environment)
            .unwrap()
            .created
            .is_empty()
    );
    let config = load_config(path).unwrap();
    assert_eq!(
        config.skills.directories,
        [root.path().join("private/skills").display().to_string()]
    );
    assert_eq!(
        config.skills.approved_roots,
        [
            root.path().join("private/skills").display().to_string(),
            root.path()
                .join("home/.claude/skills")
                .display()
                .to_string(),
        ]
    );
}

#[test]
fn expands_paths_and_checks_component_containment() {
    let root = tempfile::tempdir().unwrap();
    let environment = environment(root.path());
    assert_eq!(
        expand_path_with_env("~/code/../work", &environment).unwrap(),
        root.path().join("home/work")
    );
    let approved = root.path().join("approved");
    fs::create_dir_all(&approved).unwrap();
    let roots = canonical_roots(&[approved.display().to_string()]).unwrap();
    assert!(is_within(roots[0].join("project"), &roots));
    assert!(!is_within(
        root.path().join("approved-sibling/project"),
        &roots
    ));
}
