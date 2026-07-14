use std::process::Command;

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
