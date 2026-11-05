//! Giving the memory the windows used back to the system once they are gone.

/// Returns to the system what the allocator holds free. glibc's heaps only shrink from
/// their top, so what the windows freed (layouts, the reader's tiles, the renderer's
/// buffers) stays the process's, scattered among what is still in use: an app left
/// running with no window would keep the most it ever used. Elsewhere a no-op: other
/// allocators give it back on their own.
pub fn release_freed() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    glibc::release_freed();
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
mod glibc {
    pub fn release_freed() {
        let before = resident_kib();
        // SAFETY: malloc_trim takes no pointers and locks each heap while it trims it, so
        // other threads may allocate meanwhile.
        let released = unsafe { libc::malloc_trim(0) } == 1;
        tracing::info!(
            released,
            rss_before_kib = before,
            rss_after_kib = resident_kib(),
            "returned freed memory to the system"
        );
    }

    /// The process's resident memory, as Linux tells it.
    fn resident_kib() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
        line.split_whitespace().nth(1)?.parse().ok()
    }
}
