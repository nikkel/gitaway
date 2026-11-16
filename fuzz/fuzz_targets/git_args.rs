//! The arguments of a git command run through gitaway, separated by NUL
//! bytes.
#![no_main]

use std::ffi::OsString;

use gitaway::git::classify::classify;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let args: Vec<OsString> = text.split('\0').map(OsString::from).collect();
    classify(&args);
});
