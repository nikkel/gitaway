//! Git remote URLs, as found in a repository's config.
#![no_main]

use gitaway::git::remote_url;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|url: &str| {
    remote_url::parse(url);
});
