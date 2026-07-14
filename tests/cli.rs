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
