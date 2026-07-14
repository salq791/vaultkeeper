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
    let add = bin()
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
            r#"{"password":"pw"}"#,
        ])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let add_stderr = String::from_utf8_lossy(&add.stderr);
    assert!(add_stderr.contains("warning: inline --secrets-json"));

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

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("healthchecks configured"));
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
        .args([
            "restore",
            "--source",
            "ghost",
            "--target",
            "postgres://u:p@x.example.com/db",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ghost"));
}
