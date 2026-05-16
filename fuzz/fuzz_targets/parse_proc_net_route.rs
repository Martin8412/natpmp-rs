#![no_main]
//! Fuzz target: feed arbitrary bytes as the content of `/proc/net/route` and
//! verify the parser never panics.
//!
//! Run for 60 seconds:
//!   cargo +nightly fuzz run parse_proc_net_route -- -max_total_time=60
//!
//! Only meaningful on Linux (the function is cfg-gated), but the target
//! compiles on any platform because of the cfg guard below.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    #[cfg(target_os = "linux")]
    {
        // The parser operates on text; convert lossily so all byte sequences
        // are exercised rather than rejecting non-UTF-8 immediately.
        let text = String::from_utf8_lossy(data);
        let _ = natpmp::parse_proc_net_route(&text);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = data;
});
