//! The SOCKS5 handshake, as any program on this computer can send it to
//! gitaway's proxy. Besides not panicking, it must never accept a client
//! that didn't send the token.
#![no_main]

use std::sync::LazyLock;

use gitaway::proxy::socks::accept;
use libfuzzer_sys::fuzz_target;
use tokio::runtime::Runtime;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
});

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fuzz_target!(|data: &[u8]| {
    // The client's bytes in, the proxy's answers thrown away.
    let mut stream = tokio::io::join(data, tokio::io::sink());
    if RUNTIME.block_on(accept(&mut stream, TOKEN)).is_ok() {
        assert!(
            data.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()),
            "accepted without the token"
        );
    }
});
