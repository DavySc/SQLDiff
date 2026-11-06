//! Giving freed memory back to the system.
//!
//! A conversion allocates a great deal in its worker threads (syntax trees,
//! engine state) and frees it again, but the C allocator on Linux (glibc)
//! keeps much of what the threads freed in their own arenas, so the process
//! looks several times larger than what it holds. On a 44 MB tree the
//! resident size was 553 MB after the checks and 246 MB after asking for the
//! memory back; the run's peak fell from 972 MB to 570 MB. Elsewhere the
//! allocator returns memory by itself, and [`release`] does nothing.

/// Asks the allocator to return the memory it holds but nothing uses. Called
/// between phases, never inside one.
pub fn release() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: `malloc_trim` takes a plain integer, touches only the
        // allocator's own bookkeeping and may be called at any time.
        unsafe {
            malloc_trim(0);
        }
    }
}
