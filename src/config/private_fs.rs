//! Writing private files safely: atomically, and readable only by the user.
//!
//! Both the configuration store and the device-key file store write through
//! [`write_atomic`], so they share the same guarantees:
//!
//! - **No half-written files.** The new contents go to a temporary file in
//!   the same directory, are flushed to disk, then renamed over the old file.
//!   A rename within one directory is atomic, so after a crash or power cut
//!   the file holds either the old contents or the new, never a mix.
//! - **Private.** On Unix, files are created with mode 600 and directories
//!   with mode 700.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Replaces `path` with `contents` atomically, creating parent directories
/// (mode 700 on Unix) if needed. The temporary file is mode 600 on Unix and
/// is deleted automatically if anything fails before the rename.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("no parent directory"))?;
    create_private_dir(parent)?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Creates `dir` and any missing parents, with mode 700 on Unix.
pub(crate) fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}
