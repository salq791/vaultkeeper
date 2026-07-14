/// Recursively walk `root` for the first file or directory entry named `name`.
/// Restic restores recreate the original absolute path under `dest`, so
/// engines locate their payload this way after a restore.
pub fn find_named(root: &std::path::Path, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy() == name {
                return Ok(entry.path());
            }
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    anyhow::bail!("could not find '{name}' under {}", root.display())
}

/// Recursively count files and total bytes under `root`.
pub fn dir_stats(root: &std::path::Path) -> anyhow::Result<(u64, u64)> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                files += 1;
                bytes += entry.metadata()?.len();
            }
        }
    }
    Ok((files, bytes))
}

pub fn truncate_marked(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push_str(" ...[truncated]");
    }
    out
}

/// Create `path` atomically with owner-only permissions on unix and write `bytes`.
/// create_new refuses pre-existing files and symlinks.
pub fn write_new_0600(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[derive(Debug)]
pub struct ChildOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Run a child with piped output and a hard deadline. Reader threads drain
/// stdout/stderr so a chatty child cannot deadlock on a full pipe; on
/// timeout the child is killed and an error names the program and deadline.
pub fn output_with_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> anyhow::Result<ChildOutput> {
    use anyhow::Context;
    use std::io::Read;
    use wait_timeout::ChildExt;

    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn {program}"))?;

    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let status = match child
        .wait_timeout(timeout)
        .context("wait on child failed")?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            // Do not join the readers: a grandchild holding the inherited pipe
            // write end would block EOF forever. Output is discarded on this
            // path; detached readers exit when the pipes finally close.
            drop(out_thread);
            drop(err_thread);
            anyhow::bail!(
                "{program} timed out after {}s and was killed",
                timeout.as_secs()
            );
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    Ok(ChildOutput {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_unchanged() {
        assert_eq!(truncate_marked("abc", 5), "abc");
        assert_eq!(truncate_marked("abcde", 5), "abcde");
    }

    #[test]
    fn long_input_truncated_with_marker() {
        assert_eq!(truncate_marked("abcdef", 5), "abcde ...[truncated]");
    }

    #[test]
    fn write_new_0600_writes_bytes_and_file_exists_with_exact_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        write_new_0600(&path, b"hello secret").unwrap();
        let contents = std::fs::read(&path).unwrap();
        assert_eq!(contents, b"hello secret");
    }

    #[test]
    fn write_new_0600_refuses_collision_on_second_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        write_new_0600(&path, b"first").unwrap();
        let result = write_new_0600(&path, b"second");
        assert!(result.is_err());
        // original contents must be untouched by the refused second write
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[cfg(unix)]
    #[test]
    fn write_new_0600_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        write_new_0600(&path, b"hello secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    fn shell(script: &str) -> std::process::Command {
        #[cfg(windows)]
        {
            let mut c = std::process::Command::new("cmd");
            c.arg("/C").arg(script);
            c
        }
        #[cfg(not(windows))]
        {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(script);
            c
        }
    }

    #[test]
    fn fast_child_completes_with_output() {
        let out =
            output_with_timeout(&mut shell("echo hi"), std::time::Duration::from_secs(30)).unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
    }

    #[test]
    fn failing_child_reports_status_and_stderr() {
        let out = output_with_timeout(
            &mut shell("echo oops 1>&2 & exit 3"),
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("oops"));
    }

    #[test]
    fn dir_stats_counts_files_and_bytes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("a.bin"), b"12345").unwrap();
        std::fs::write(d.path().join("sub").join("b.bin"), b"123").unwrap();
        assert_eq!(dir_stats(d.path()).unwrap(), (2, 8));
    }

    #[test]
    fn find_named_locates_nested_entry() {
        let d = tempfile::tempdir().unwrap();
        let deep = d.path().join("a").join("b").join("target-dir");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_named(d.path(), "target-dir").unwrap(), deep);
        assert!(find_named(d.path(), "missing").is_err());
    }

    #[test]
    fn hung_child_is_killed_and_errors() {
        // Spawn the long-running program directly rather than through the shell
        // helper: `cmd /C "ping ... > NUL"` makes ping.exe a *grandchild* of the
        // killed process, and Windows handle inheritance leaves ping.exe holding
        // a duplicate of our stdout pipe's write end, so our reader thread blocks
        // until ping.exe itself exits (measured: ~58s), even though kill()/wait()
        // on the immediate cmd.exe child return in low milliseconds. Spawning the
        // real binary directly (as production callers in Task 2 will) makes it
        // the immediate child, so kill() closes its pipe handles right away.
        #[cfg(windows)]
        let mut cmd = {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = shell("sleep 60");
        let start = std::time::Instant::now();
        let err = output_with_timeout(&mut cmd, std::time::Duration::from_secs(1)).unwrap_err();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "must not wait for the child"
        );
        assert!(err.to_string().contains("timed out"));
    }
}
