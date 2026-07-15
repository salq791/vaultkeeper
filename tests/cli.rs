use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vaultkeeper"))
}

const K: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[test]
fn source_add_then_list_shows_source_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vk.db");
    let mut child = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "acme-db",
            "--engine",
            "postgres",
            "--schedule",
            "0 2 * * *",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"password":"pw"}"#)
        .unwrap();
    let add = child.wait_with_output().unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );

    let list = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args(["source", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("acme-db"));
    assert!(stdout.contains("postgres"));
    assert!(!stdout.contains("pw"), "secrets must never be printed");
}

#[test]
fn source_add_refuses_inline_secrets_without_echoing_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vk.db");
    let output = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "acme-db",
            "--engine",
            "postgres",
            "--schedule",
            "0 2 * * *",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json",
            r#"{"password":"do-not-echo"}"#,
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("process arguments"));
    assert!(!stderr.contains("do-not-echo"));
}

#[test]
fn source_add_reads_secrets_from_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vk.db");
    let mut child = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "acme-db",
            "--engine",
            "postgres",
            "--schedule",
            "0 2 * * *",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"password":"stdinpw"}"#)
        .unwrap();
    let add = child.wait_with_output().unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(!String::from_utf8_lossy(&add.stderr).contains("warning:"));

    let list = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args(["source", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("acme-db"));
    assert!(!stdout.contains("stdinpw"), "secrets must never be printed");
}

#[test]
fn source_disable_then_list_shows_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vk.db");
    let mut child = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "acme-db",
            "--engine",
            "postgres",
            "--schedule",
            "0 2 * * *",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"password":"pw"}"#)
        .unwrap();
    let add = child.wait_with_output().unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );

    let disable = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args(["source", "disable", "--name", "acme-db"])
        .output()
        .unwrap();
    assert!(
        disable.status.success(),
        "{}",
        String::from_utf8_lossy(&disable.stderr)
    );

    let list = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args(["source", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("disabled"));

    let bad_schedule = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "other-db",
            "--engine",
            "postgres",
            "--schedule",
            "banana",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json",
            r#"{"password":"pw"}"#,
        ])
        .output()
        .unwrap();
    assert!(!bad_schedule.status.success());
    assert!(String::from_utf8_lossy(&bad_schedule.stderr).contains("banana"));
}

#[test]
fn source_add_accepts_verify_schedule_and_verify_hc_uuid_and_validates_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vk.db");

    let mut child = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "acme-db",
            "--engine",
            "postgres",
            "--schedule",
            "0 2 * * *",
            "--verify-schedule",
            "0 5 * * 0",
            "--verify-healthchecks-uuid",
            "hc-test-uuid",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"password":"pw"}"#)
        .unwrap();
    let add = child.wait_with_output().unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );

    let bad = bin()
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "other-db",
            "--engine",
            "postgres",
            "--schedule",
            "0 2 * * *",
            "--verify-schedule",
            "banana",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json",
            r#"{"password":"pw"}"#,
        ])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("banana"));
}

#[test]
fn check_config_reports_notify_channels() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    let db = dir.path().join("vk.db");

    let config_content = r#"
[global]
staging_dir = "/staging"
restic_repo = "local:/tmp/restic"
restic_password = "testpw"

[notify]
healthchecks_base = "https://hc-ping.com"
"#;

    std::fs::write(&config_file, config_content).unwrap();

    let output = bin()
        .env("VAULTKEEPER_CONFIG", &config_file)
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args(["check-config"])
        .output()
        .unwrap();

    // Exit status is intentionally NOT asserted here: check-config now exits
    // nonzero when a required tool is MISSING from PATH, and dev machines
    // running this suite may not have every engine tool installed. The
    // dedicated exit-code test below (check_config_fails_on_missing_tools)
    // pins PATH to an empty dir so the nonzero-exit behavior is exercised
    // deterministically.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("healthchecks configured"));
}

#[test]
fn check_config_fails_on_missing_tools() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    let db = dir.path().join("vk.db");
    let empty_path_dir = dir.path().join("empty-path");
    std::fs::create_dir_all(&empty_path_dir).unwrap();

    let config_content = r#"
[global]
staging_dir = "/staging"
restic_repo = "local:/tmp/restic"
restic_password = "testpw"
"#;
    std::fs::write(&config_file, config_content).unwrap();

    let output = bin()
        .env("VAULTKEEPER_CONFIG", &config_file)
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .env("PATH", &empty_path_dir)
        .args(["check-config"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("MISSING"));
}

#[test]
fn restore_requires_known_source() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    let db = dir.path().join("vk.db");

    let config_content = r#"
[global]
staging_dir = "/staging"
restic_repo = "local:/tmp/restic"
restic_password = "testpw"
"#;
    std::fs::write(&config_file, config_content).unwrap();

    let output = bin()
        .env("VAULTKEEPER_CONFIG", &config_file)
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .env(
            "VAULTKEEPER_RESTORE_TARGET",
            "postgres://u:p@x.example.com/db",
        )
        .args(["restore", "--source", "ghost"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ghost"));
}

#[test]
fn restore_reads_target_from_environment() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    let db = dir.path().join("vk.db");

    let config_content = r#"
[global]
staging_dir = "/staging"
restic_repo = "local:/tmp/restic"
restic_password = "testpw"
"#;
    std::fs::write(&config_file, config_content).unwrap();

    let output = bin()
        .env("VAULTKEEPER_CONFIG", &config_file)
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .env(
            "VAULTKEEPER_RESTORE_TARGET",
            "postgres://u:p@elsewhere.example.com:5432/db",
        )
        .args(["restore", "--source", "ghost"])
        .output()
        .unwrap();

    // The environment-only target is wired through without erroring before
    // the expected unknown-source failure.
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ghost"));
}

#[test]
fn check_config_flags_bad_timeout_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    let db = dir.path().join("vk.db");

    let config_content = r#"
[global]
staging_dir = "/staging"
restic_repo = "local:/tmp/restic"
restic_password = "testpw"
"#;
    std::fs::write(&config_file, config_content).unwrap();

    // New writes reject an invalid timeout immediately.
    let mut child = bin()
        .env("VAULTKEEPER_CONFIG", &config_file)
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "test-db",
            "--engine",
            "postgres",
            "--schedule",
            "0 2 * * *",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres","timeout_minutes":"soon"}"#,
            "--secrets-json",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"password":"pw"}"#)
        .unwrap();
    let add = child.wait_with_output().unwrap();
    assert!(!add.status.success());
    assert!(String::from_utf8_lossy(&add.stderr).contains("timeout_minutes"));

    // Add a valid row, then emulate a malformed row written by an older
    // release so check-config's compatibility audit remains covered.
    let mut child = bin()
        .env("VAULTKEEPER_CONFIG", &config_file)
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args([
            "source",
            "add",
            "--name",
            "test-db",
            "--engine",
            "postgres",
            "--schedule",
            "0 2 * * *",
            "--settings-json",
            r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres"}"#,
            "--secrets-json",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"password":"pw"}"#)
        .unwrap();
    let valid_add = child.wait_with_output().unwrap();
    assert!(valid_add.status.success());
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE sources SET settings_json = ?1 WHERE name = 'test-db'",
            [r#"{"host":"db.example.com","port":5432,"dbname":"app","user":"postgres","timeout_minutes":"soon"}"#],
        )
        .unwrap();

    // check-config still detects malformed legacy/database-edited rows.
    let output = bin()
        .env("VAULTKEEPER_CONFIG", &config_file)
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .args(["check-config"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("timeout_minutes INVALID"),
        "stdout: {stdout}"
    );
    assert!(!output.status.success(), "check-config should fail");
}
