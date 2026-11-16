//! Profile and global config files, and the errors that describe them.
#![no_main]

use gitaway::config::profile::{GlobalConfig, Profile};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    if let Err(e) = Profile::from_toml(text) {
        e.to_string();
    }
    if let Err(e) = GlobalConfig::from_toml(text) {
        e.to_string();
    }
});
