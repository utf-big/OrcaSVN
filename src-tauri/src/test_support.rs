//! Helpers shared by the tests.

/// A value that is unique inside this test process.
///
/// Tests run in parallel and `SystemTime::now()` is too coarse on some platforms:
/// two tests that started within the same tick were handed the same temporary
/// directory and clobbered each other's files, so a suite that passed alone failed
/// when the whole binary ran. The counter makes the value monotonic per test.
pub(crate) fn unique_temp_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    (nanos as u64) ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}
