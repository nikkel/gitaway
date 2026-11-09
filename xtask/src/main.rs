//! Maintenance tasks for gitaway, run with `cargo xtask <task>`.
//!
//! Tasks:
//!
//! - `man [--out <dir>]`: write the man pages into `target/man`, or `<dir>`.
//!   Package builds (release archives, Homebrew) run this and install the
//!   pages. They aren't committed: `cargo install` can't install man pages,
//!   so only packaged installs have them.
//!
//! # Extending
//!
//! A new task is a new arm in `main` and a module for it.

mod man;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["man"] => man::write(&repo_root().join("target").join("man")),
        ["man", "--out", dir] => man::write(Path::new(dir)),
        _ => Err("usage: cargo xtask man [--out <dir>]".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The repository root, wherever `cargo xtask` was run from.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the repository")
        .to_path_buf()
}
