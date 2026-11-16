//! A VPN client config, as pasted into `gitaway setup` or given with
//! `--config`.
#![no_main]

use gitaway::transport::client_config;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    if let Ok(config) = client_config::read(text) {
        config.likely_addresses("git.example.com");
        config.likely_addresses("192.0.2.10");
    }
});
