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
}
