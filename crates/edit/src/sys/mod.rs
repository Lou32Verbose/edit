// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Platform abstractions.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
pub use std::fs::canonicalize;

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_file_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        path.push(format!("edit32-sys-{name}-{}-{nonce}.tmp", std::process::id()));
        path
    }

    #[test]
    fn atomic_write_overwrites_existing_file() {
        let path = temp_file_path("atomic-write");
        atomic_write(&path, |f| {
            f.write_all(b"one")?;
            Ok(())
        })
        .expect("first atomic write failed");
        atomic_write(&path, |f| {
            f.write_all(b"two")?;
            Ok(())
        })
        .expect("second atomic write failed");

        let contents = fs::read_to_string(&path).expect("failed to read file");
        assert_eq!(contents, "two");

        let _ = fs::remove_file(path);
    }
}
