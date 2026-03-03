// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_TMP_ATTEMPTS: u32 = 256;

pub fn atomic_temp_path(parent: &Path, file_name: &OsStr, attempt: u32) -> io::Result<PathBuf> {
    if attempt > MAX_TMP_ATTEMPTS {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create a unique temporary file for atomic write",
        ));
    }

    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);

    let mut name = OsString::from(file_name);
    name.push(".tmp-");
    name.push(format!("{}-{nonce:016x}-{attempt}", std::process::id()));
    Ok(parent.join(name))
}
