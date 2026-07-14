//! Runs only where restic is installed: `cargo test --test e2e_restic -- --ignored`
use std::process::Command;

const K: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[test]
#[ignore = "requires restic on PATH; runs in CI"]
fn full_backup_into_local_restic_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let staging = dir.path().join("staging");
    let db = dir.path().join("vk.db");
    let cfg_path = dir.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[global]\nstaging_dir = \"{}\"\nrestic_repo = \"{}\"\nrestic_password = \"testpw\"\n",
            staging.display().to_string().replace('\\', "/"),
            repo.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();

    // fake pg_dump: a shim directory prepended to PATH that writes a file
    let shim = dir.path().join("shim");
    std::fs::create_dir_all(&shim).unwrap();
    let script = shim.join("pg_dump");
    std::fs::write(
        &script,
        "#!/bin/sh\nwhile [ \"$1\" != \"-f\" ]; do shift; done\necho fakedump > \"$2\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_env = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_vaultkeeper"))
            .env("VAULTKEEPER_MASTER_KEY", K)
            .env("VAULTKEEPER_DB", &db)
            .env("VAULTKEEPER_CONFIG", &cfg_path)
            .env("PATH", &path_env)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    run(&[
        "source",
        "add",
        "--name",
        "e2e-db",
        "--engine",
        "postgres",
        "--schedule",
        "0 2 * * *",
        "--settings-json",
        r#"{"host":"localhost","port":5432,"dbname":"app","user":"postgres"}"#,
        "--secrets-json",
        r#"{"password":"x"}"#,
    ]);
    let out = run(&["run", "--source", "e2e-db"]);
    assert!(out.contains("snapshot"));
    let snaps = run(&["snapshots", "--source", "e2e-db"]);
    assert!(snaps.contains("source=e2e-db"));
}
