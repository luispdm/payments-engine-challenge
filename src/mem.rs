//! Process-level memory metrics for the per-variant memory-bench bins.
//!
//! Lives in the library so both `mem_v1` and `mem_v2` share the same RSS
//! reader. Gated behind the `bench` feature: the production build pulls
//! in no `libc` dependency.

/// Read the process's peak resident set size (in kibibytes) via
/// `getrusage(RUSAGE_SELF)`. On Linux `ru_maxrss` is reported in kB; on
/// macOS it's bytes (the bench scripts run on Linux so the kB
/// interpretation is the binding one). The value is a high-water mark
/// since process start, so callers should snapshot it once after the
/// workload has been fully ingested.
#[must_use]
pub fn peak_rss_kb() -> i64 {
    // SAFETY: `getrusage` is a thread-safe libc call; we pass a properly
    // sized `rusage` struct allocated on the stack and only read the
    // `ru_maxrss` field that the kernel populates. No invariants of the
    // surrounding program are touched.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        let rc = libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        assert_eq!(rc, 0, "getrusage(RUSAGE_SELF) failed");
        usage.ru_maxrss
    }
}
