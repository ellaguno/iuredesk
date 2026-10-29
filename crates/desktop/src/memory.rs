//! Memoria residente del proceso donde no hay `/proc` (R2.1 pendiente → R1.2): Windows
//! (`GetProcessMemoryInfo`, «espacio de trabajo») y macOS (`task_info`, residente), por
//! `memory-stats` (enlaces en Rust a las llamadas del sistema). En Linux la UI ya lee
//! `/proc/self/status`, que además da la memoria anónima y el pico.

/// Bytes residentes del proceso, si el sistema lo dice.
pub fn resident_bytes() -> Option<u64> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        memory_stats::memory_stats().and_then(|m| u64::try_from(m.physical_mem).ok())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}
