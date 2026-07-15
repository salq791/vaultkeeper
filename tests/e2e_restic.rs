//! Runs only where restic is installed: `cargo test --test e2e_restic -- --ignored`
use std::io::Write;
use std::process::{Command, Stdio};

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
            "[global]\nstaging_dir = \"{}\"\nrestic_repo = \"{}\"\nrestic_password = \"testpw\"\nsecret_temp_dir = \"{}\"\n",
            staging.display().to_string().replace('\\', "/"),
            repo.display().to_string().replace('\\', "/"),
            dir.path().join("secrets").display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();

    // fake pg_dump: a shim directory prepended to PATH that writes a file
    let shim = dir.path().join("shim");
    std::fs::create_dir_all(&shim).unwrap();
    let script = shim.join("pg_dump");
    std::fs::write(
        &script,
        "#!/bin/sh\nwhile [ \"$1\" != \"-f\" ]; do\n  [ -z \"$1\" ] && { echo \"shim: missing -f\" >&2; exit 1; }\n  shift\ndone\necho fakedump > \"$2\"\n",
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

    let run_with_stdin = |args: &[&str], stdin: &[u8]| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_vaultkeeper"))
            .env("VAULTKEEPER_MASTER_KEY", K)
            .env("VAULTKEEPER_DB", &db)
            .env("VAULTKEEPER_CONFIG", &cfg_path)
            .env("PATH", &path_env)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    run(&["init-repository"]);
    run_with_stdin(
        &[
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
            "-",
        ],
        br#"{"password":"x"}"#,
    );
    let out = run(&["run", "--source", "e2e-db"]);
    assert!(out.contains("snapshot"));
    let snaps = run(&["snapshots", "--source", "e2e-db"]);
    assert!(snaps.contains("source=e2e-db"));

    // restore roundtrip with shims: pg_restore records argv, psql answers queries
    let pg_restore = shim.join("pg_restore");
    std::fs::write(
        &pg_restore,
        "#!/bin/sh\necho \"$@\" > \"$SHIM_MARKER\"\nexit 0\n",
    )
    .unwrap();
    let psql = shim.join("psql");
    std::fs::write(&psql, "#!/bin/sh\necho 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pg_restore, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&psql, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let marker = dir.path().join("pg_restore_argv.txt");
    let out = Command::new(env!("CARGO_BIN_EXE_vaultkeeper"))
        .env("VAULTKEEPER_MASTER_KEY", K)
        .env("VAULTKEEPER_DB", &db)
        .env("VAULTKEEPER_CONFIG", &cfg_path)
        .env("PATH", &path_env)
        .env("SHIM_MARKER", &marker)
        .env(
            "VAULTKEEPER_RESTORE_TARGET",
            "postgres://u:pw@elsewhere.example.com:5432/restored",
        )
        .args([
            "restore",
            "--source",
            "e2e-db",
            "--confirm-source",
            "e2e-db",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "restore failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let argv = std::fs::read_to_string(&marker).unwrap();
    assert!(argv.contains("--clean"));
    assert!(argv.contains("--single-transaction"));
    assert!(argv.contains("-d restored"));
    assert!(
        !argv.contains("pw"),
        "password must never reach pg_restore argv"
    );
}
