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
//!   with mode 700. gitaway's configuration directory (the `root` every
//!   function takes) is made 700 even if something else created it first,
//!   such as an installer writing its receipt there with the default 755.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Replaces `path` with `contents` atomically. `path` is inside `root`,
/// gitaway's configuration directory: see [`create_private_dir`] for how the
/// directories are created. The temporary file is mode 600 on Unix and is
/// deleted automatically if anything fails before the rename.
pub(crate) fn write_atomic(root: &Path, path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("no parent directory"))?;
    create_private_dir(root, parent)?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Creates `dir`, which is `root` or inside it, and any missing parents,
/// with mode 700 on Unix. `root` is gitaway's configuration directory; if it
/// already exists and other users can read it, it's made 700.
pub(crate) fn create_private_dir(root: &Path, dir: &Path) -> io::Result<()> {
    create_dirs(root)?;
    make_private(root)?;
    create_dirs(dir)
}

fn create_dirs(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

#[cfg(unix)]
fn make_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_private(_dir: &Path) -> io::Result<()> {
    Ok(())
}
