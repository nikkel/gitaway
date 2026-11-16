//! The start of a connection to `gitaway tunnel`'s local web port, which any
//! web page can make the browser send. Besides not panicking or reading
//! without limit, what it forwards must be exactly the bytes it read.
#![no_main]

use std::sync::LazyLock;

use gitaway::cli::web_guard::{Opening, read_opening};
use libfuzzer_sys::fuzz_target;
use tokio::runtime::Runtime;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
});

fuzz_target!(|data: &[u8]| {
    let Some((port, request)) = data.split_first_chunk::<2>() else {
        return;
    };
    let port = u16::from_be_bytes(*port);
    // Reading moves the slice past the bytes read.
    let mut stream = request;
    let opening = RUNTIME.block_on(read_opening(&mut stream, port));
    if let Opening::Forward(head) = opening {
        let read = &request[..request.len() - stream.len()];
        assert_eq!(head, read, "forwarded other bytes than it read");
    }
});
