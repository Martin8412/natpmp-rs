#![no_main]
//! Fuzz target: feed arbitrary bytes as a NAT-PMP response and verify the
//! parser never panics — it must always return Ok or Err, never abort.
//!
//! Run for 60 seconds:
//!   cargo +nightly fuzz run parse_natpmp_response -- -max_total_time=60
//!
//! The three response opcodes (128 public-addr, 129 UDP map, 130 TCP map) are
//! all exercised so the minimum-length check (12 vs 16) gets both paths.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Public-address response (min 12 bytes)
    let _ = natpmp::validate_response_bytes(data, 128);
    // UDP port-mapping response (min 16 bytes)
    let _ = natpmp::validate_response_bytes(data, 129);
    // TCP port-mapping response (min 16 bytes)
    let _ = natpmp::validate_response_bytes(data, 130);
});
