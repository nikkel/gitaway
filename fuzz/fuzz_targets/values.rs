//! Single values from profiles and the command line: the first byte picks
//! which parser gets the rest.
#![no_main]

use gitaway::address::HostPort;
use gitaway::config::name::ProfileName;
use gitaway::config::value::{HostKey, InterfaceAddress, WebUrl, WireGuardPublicKey};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&which, rest)) = data.split_first() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    match which % 6 {
        0 => drop(text.parse::<HostPort>()),
        1 => drop(text.parse::<HostKey>()),
        2 => drop(text.parse::<WireGuardPublicKey>()),
        3 => drop(text.parse::<InterfaceAddress>()),
        4 => drop(text.parse::<WebUrl>()),
        _ => drop(text.parse::<ProfileName>()),
    }
});
