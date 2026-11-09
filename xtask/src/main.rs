//! Maintenance tasks for gitaway, run with `cargo xtask <task>`.
//!
//! Tasks:
//!
//! - `man`: write the man pages into `man/`.
//! - `man --check`: fail if `man/` doesn't match what `man` would write. CI
//!   runs this, so the committed pages can't fall behind the code.
//!
//! The pages are committed so they can be reviewed in a diff and read
//! straight from a checkout (`man ./man/gitaway.1`).
//!
//! # Extending
//!
//! A new task is a new arm in `main` and a module for it.

mod man;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["man"] => man::write(&man_dir()),
        ["man", "--check"] => man::check(&man_dir()),
        _ => Err("usage: cargo xtask man [--check]".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

/// `man/` at the repository root, wherever `cargo xtask` was run from.
fn man_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the repository")
        .join("man")
}
