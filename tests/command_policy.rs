use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use intake::agent::command_policy::{CommandPolicy, MAX_COMMAND_STDIN_BYTES};
use intake::config::{CommandRule, load_config};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct Fixture {
    _root: TempDir,
    root: PathBuf,
    bin: PathBuf,
    marker: PathBuf,
    policy: CommandPolicy,
}

fn fixture() -> Fixture {
    fixture_with_timeout(30)
}

fn fixture_with_timeout(timeout_seconds: u64) -> Fixture {
    let root = TempDir::new().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let marker = root.path().join("calls.log");
    fs::write(&marker, "").unwrap();
    for name in [
        "allowed",
        "capture",
        "large",
        "slow",
        "ignore-term",
        "orphan",
        "cat",
    ] {
        let body = match name {
            "allowed" => format!(
                "#!/bin/sh\nprintf 'allowed' >> '{}'\nfor argument in \"$@\"; do printf '|%s' \"$argument\" >> '{}'; done\nprintf '\\n' >> '{}'\n/bin/cat\nprintf 'token=secret-value\\n'\n",
                marker.display(),
                marker.display(),
                marker.display()
            ),
            "capture" => "#!/bin/sh\n/bin/cat\n".to_string(),
            "large" => {
                "#!/bin/sh\ni=0; while [ $i -lt 400 ]; do printf '0123456789'; i=$((i+1)); done\n"
                    .to_string()
            }
            "slow" => "#!/bin/sh\n/bin/sleep 60\n".to_string(),
            "ignore-term" => format!(
                "#!/bin/sh\ntrap '' TERM\n/bin/sh -c 'trap \"\" TERM; while :; do /bin/sleep 1; done' &\nprintf '%s' $! > '{}.pid'\nwait\n",
                marker.display()
            ),
            "orphan" => format!(
                "#!/bin/sh\n/bin/sh -c 'trap \"\" TERM; while :; do /bin/sleep 1; done' &\nprintf '%s' $! > '{}.orphan.pid'\nexit 0\n",
                marker.display()
            ),
            "cat" => "#!/bin/sh\nexec /bin/cat\n".to_string(),
            _ => unreachable!(),
        };
        write_executable(&bin.join(name), &body);
    }
    let mut config =
        load_config(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config/valid.yaml"))
            .unwrap();
    config.commands.path = vec![bin.display().to_string()];
    config.commands.timeout_seconds = timeout_seconds;
    config.commands.max_output_bytes = 1024;
    config.commands.rules = [
        "allowed",
        "capture",
        "large",
        "slow",
        "ignore-term",
        "orphan",
        "cat",
    ]
    .into_iter()
    .map(|executable| CommandRule {
        executable: executable.into(),
    })
    .collect();
    let root_path = fs::canonicalize(root.path()).unwrap();
    let policy = CommandPolicy::new(&config, vec![root_path.clone()]).unwrap();
    Fixture {
        _root: root,
        root: root_path,
        bin,
        marker,
        policy,
    }
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn accepts_literal_arguments_and_authorized_pipelines() {
    let fixture = fixture();
    let parsed = fixture
        .policy
        .parse_and_authorize(
            "allowed search \"login issue\" | capture 'APP-*'",
            &fixture.root,
        )
        .unwrap();
    assert_eq!(
        parsed.stages,
        [
            vec!["allowed", "search", "login issue"],
            vec!["capture", "APP-*"]
        ]
    );
    let result = fixture
        .policy
        .execute(
            "allowed search \"login issue\" | capture 'APP-*'",
            &fixture.root,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("[REDACTED]"));
    assert!(
        fs::read_to_string(&fixture.marker)
            .unwrap()
            .contains("allowed|search|login issue")
    );
}

#[tokio::test]
async fn passes_bounded_multiline_stdin_without_a_shell() {
    let fixture = fixture();
    let input = "First paragraph.\n\nSecond paragraph.";
    let result = fixture
        .policy
        .execute("cat", &fixture.root, CancellationToken::new(), Some(input))
        .await
        .unwrap();
    assert_eq!(result.stdout, input);
    let oversized = "x".repeat(MAX_COMMAND_STDIN_BYTES + 1);
    assert!(
        fixture
            .policy
            .execute(
                "cat",
                &fixture.root,
                CancellationToken::new(),
                Some(&oversized),
            )
            .await
            .is_err()
    );
}

#[test]
fn preserves_quoted_newlines_and_literal_escaped_operators() {
    let fixture = fixture();
    let command =
        "allowed add task -p \"Investigate this email.\n\n<untrusted>\nBody\n</untrusted>\"";
    assert_eq!(
        fixture
            .policy
            .parse_and_authorize(command, &fixture.root)
            .unwrap()
            .stages[0][4],
        "Investigate this email.\n\n<untrusted>\nBody\n</untrusted>"
    );
    assert_eq!(
        fixture
            .policy
            .parse_and_authorize(r"allowed literal\;operator \$HOME", &fixture.root)
            .unwrap()
            .stages[0],
        ["allowed", "literal;operator", "$HOME"]
    );
}

#[test]
fn rejects_every_unsupported_or_recovered_syntax_node() {
    let fixture = fixture();
    let forbidden = [
        "allowed x; capture y",
        "allowed x && capture y",
        "allowed $(capture)",
        "allowed `capture`",
        "allowed $HOME",
        "allowed ${HOME}",
        "allowed $((1+1))",
        "allowed x > out",
        "VALUE=x allowed y",
        "allowed *.md",
        "allowed ?.md",
        "allowed [ab]",
        "(allowed x)",
        "allowed x;",
        "allowed x # hidden",
        "allowed x &",
        "allowed x\ncapture y",
        "allowed x\r\ncapture y",
        "allowed <(capture)",
        "allowed >(capture)",
        "allowed $'ansi'",
        "allowed <<< value",
        "allowed <<EOF\nvalue\nEOF",
        "allowed \"unterminated",
        "allowed trailing\\",
        "allowed | | capture",
        "allowed || capture",
        "allowed\u{00a0}argument",
        "! allowed x",
        "allowed {1..3}",
    ];
    for command in forbidden {
        assert!(
            fixture
                .policy
                .parse_and_authorize(command, &fixture.root)
                .is_err(),
            "accepted forbidden command: {command:?}"
        );
    }
}

#[test]
fn enforces_utf16_command_and_pipeline_bounds() {
    let fixture = fixture();
    let boundary = format!("allowed {}", "🦀".repeat(16_380));
    assert_eq!(boundary.encode_utf16().count(), 32_768);
    fixture
        .policy
        .parse_and_authorize(&boundary, &fixture.root)
        .unwrap();
    assert!(
        fixture
            .policy
            .parse_and_authorize(&format!("{boundary}🦀"), &fixture.root)
            .is_err()
    );
    assert!(
        fixture
            .policy
            .parse_and_authorize("allowed nul\0byte", &fixture.root)
            .is_err()
    );
    assert!(
        fixture
            .policy
            .parse_and_authorize(
                "allowed | allowed | allowed | allowed | allowed | allowed | allowed | allowed | allowed",
                &fixture.root,
            )
            .is_err()
    );
}

#[tokio::test]
async fn authorizes_all_stages_before_resolving_or_spawning() {
    let fixture = fixture();
    let error = fixture
        .policy
        .execute(
            "allowed x | forbidden y",
            &fixture.root,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("not allowed"), "{error}");
    assert_eq!(fs::read_to_string(&fixture.marker).unwrap(), "");
}

#[tokio::test]
async fn rejects_working_directory_escapes_and_executable_symlinks() {
    let fixture = fixture();
    let outside = TempDir::new().unwrap();
    let cwd_link = fixture.root.join("cwd-link");
    std::os::unix::fs::symlink(outside.path(), &cwd_link).unwrap();
    assert!(
        fixture
            .policy
            .execute("cat", &cwd_link, CancellationToken::new(), None)
            .await
            .is_err()
    );

    fs::remove_file(fixture.bin.join("cat")).unwrap();
    std::os::unix::fs::symlink("/bin/cat", fixture.bin.join("cat")).unwrap();
    let error = fixture
        .policy
        .execute("cat", &fixture.root, CancellationToken::new(), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("unavailable on the fixed PATH"), "{error}");
}

#[tokio::test]
async fn bounds_output_and_propagates_timeout_and_cancellation() {
    let fixture = fixture();
    let output = fixture
        .policy
        .execute("large", &fixture.root, CancellationToken::new(), None)
        .await
        .unwrap();
    assert!(output.stdout.len() <= 1024);
    assert!(output.truncated);

    let timeout_fixture = fixture_with_timeout(10);
    let timeout = timeout_fixture
        .policy
        .execute(
            "slow",
            &timeout_fixture.root,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(timeout.contains("timed out"), "{timeout}");

    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let cancelled = timeout_fixture
        .policy
        .execute("slow", &timeout_fixture.root, cancellation, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(cancelled.contains("cancelled"), "{cancelled}");
}

#[tokio::test]
async fn times_out_when_an_exited_parent_leaves_pipe_holding_descendants() {
    let fixture = fixture_with_timeout(10);
    let error = fixture
        .policy
        .execute("orphan", &fixture.root, CancellationToken::new(), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("timed out"), "{error}");
    let pid: i32 = fs::read_to_string(format!("{}.orphan.pid", fixture.marker.display()))
        .unwrap()
        .parse()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
}

#[tokio::test]
async fn kills_descendants_that_ignore_sigterm() {
    let fixture = fixture_with_timeout(10);
    let error = fixture
        .policy
        .execute("ignore-term", &fixture.root, CancellationToken::new(), None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("timed out"), "{error}");
    let pid: i32 = fs::read_to_string(format!("{}.pid", fixture.marker.display()))
        .unwrap()
        .parse()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "descendant {pid} survived process-group escalation");
}
