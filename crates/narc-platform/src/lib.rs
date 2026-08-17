//! OS resource policy. Nova Arc must use every core yet never make the
//! machine feel slow, and must stay inside a memory budget so that even a
//! weak PC can extract.
//!
//! The policy (see docs/research/09-resource-scheduling.md):
//! - CPU: `BELOW_NORMAL_PRIORITY_CLASS` by default — full throughput on an
//!   idle machine, yields instantly to anything the user is doing.
//! - I/O: per-handle `Low` priority hint on bulk data handles. Not "very low"
//!   (that is 1-3% of the disk under contention and starves the archiver).
//! - Memory: below-normal memory priority, so our pages are evicted before
//!   the foreground app's.
//! - EcoQoS is opt-in only: on CPUs without efficiency cores it clamps the
//!   frequency instead of migrating work, which is the wrong default.
//!
//! `PROCESS_MODE_BACKGROUND_BEGIN` is deliberately never used: it forces I/O
//! *and* memory priority to "very low" together with a working-set squeeze.

use std::fs::File;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PriorityMode {
    /// Below-normal CPU/memory priority (default).
    #[default]
    Background,
    /// Idle priority + EcoQoS: laptops, overnight jobs.
    Eco,
    /// Normal priority, no throttling: benchmarks, dedicated machines.
    Full,
}

#[derive(Clone, Copy, Debug)]
pub struct MemoryStatus {
    pub total: u64,
    pub available: u64,
}

/// Logical cores available to this process.
pub fn logical_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Memory the pipeline may use, in bytes: half of what is free, but never
/// more than a quarter of the machine, clamped to [512 MiB, 8 GiB]. The point
/// is to stay invisible on a PC that is already half-loaded.
pub fn memory_budget(override_bytes: Option<u64>) -> u64 {
    const MIN: u64 = 512 * 1024 * 1024;
    const MAX: u64 = 8 * 1024 * 1024 * 1024;
    if let Some(b) = override_bytes {
        return b.max(64 * 1024 * 1024);
    }
    match memory_status() {
        Some(m) => (m.available / 2).min(m.total / 4).clamp(MIN, MAX),
        None => MIN,
    }
}

#[cfg(windows)]
mod imp {
    use super::{MemoryStatus, PriorityMode};
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        FileIoPriorityHintInfo, IoPriorityHintLow, LockFileEx, SetFileInformationByHandle,
        FILE_IO_PRIORITY_HINT_INFO, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, ProcessMemoryPriority, ProcessPowerThrottling, SetPriorityClass,
        SetProcessInformation, BELOW_NORMAL_PRIORITY_CLASS, IDLE_PRIORITY_CLASS,
        MEMORY_PRIORITY_BELOW_NORMAL, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW,
        MEMORY_PRIORITY_NORMAL, NORMAL_PRIORITY_CLASS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    };

    pub fn apply_process_policy(mode: PriorityMode) {
        let class = match mode {
            PriorityMode::Background => BELOW_NORMAL_PRIORITY_CLASS,
            PriorityMode::Eco => IDLE_PRIORITY_CLASS,
            PriorityMode::Full => NORMAL_PRIORITY_CLASS,
        };
        let mem = match mode {
            PriorityMode::Background => MEMORY_PRIORITY_BELOW_NORMAL,
            PriorityMode::Eco => MEMORY_PRIORITY_LOW,
            PriorityMode::Full => MEMORY_PRIORITY_NORMAL,
        };
        // SAFETY: all three calls take a pseudo-handle to the current process
        // and correctly sized, initialized structures; failures are ignored
        // because the policy is advisory.
        unsafe {
            let h = GetCurrentProcess();
            SetPriorityClass(h, class);

            let mp = MEMORY_PRIORITY_INFORMATION {
                MemoryPriority: mem,
            };
            SetProcessInformation(
                h,
                ProcessMemoryPriority,
                &mp as *const _ as *const core::ffi::c_void,
                size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
            );

            // EcoQoS on for --eco, explicitly off otherwise (a parent process
            // may have left it on).
            let pt = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
                StateMask: if mode == PriorityMode::Eco {
                    PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                } else {
                    0
                },
            };
            SetProcessInformation(
                h,
                ProcessPowerThrottling,
                &pt as *const _ as *const core::ffi::c_void,
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            );
        }
    }

    pub fn lower_io_priority(file: &File) {
        let hint = FILE_IO_PRIORITY_HINT_INFO {
            PriorityHint: IoPriorityHintLow,
        };
        // SAFETY: `file` owns a valid handle for the duration of the call and
        // the structure matches FileIoPriorityHintInfo.
        unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as _,
                FileIoPriorityHintInfo,
                &hint as *const _ as *const core::ffi::c_void,
                size_of::<FILE_IO_PRIORITY_HINT_INFO>() as u32,
            );
        }
    }

    /// Byte range used purely as a mutex. It sits far beyond any real
    /// archive, so locking it excludes other writers without blocking reads
    /// of the data - unlike a whole-file lock, which would make our own
    /// extraction threads fail with ERROR_LOCK_VIOLATION.
    const LOCK_SENTINEL: u64 = 0xFFFF_FFFF_FFFF_0000;

    pub fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.Anonymous.Anonymous.Offset = (LOCK_SENTINEL & 0xFFFF_FFFF) as u32;
        ov.Anonymous.Anonymous.OffsetHigh = (LOCK_SENTINEL >> 32) as u32;
        // SAFETY: valid handle, and `ov` is a zeroed OVERLAPPED with only the
        // offset fields set, as LockFileEx requires.
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle() as _,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut ov,
            )
        };
        if ok != 0 {
            return Ok(true);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            // ERROR_LOCK_VIOLATION / ERROR_SHARING_VIOLATION: someone else holds it.
            Some(33) | Some(32) => Ok(false),
            _ => Err(err),
        }
    }

    pub fn peak_memory() -> Option<u64> {
        let mut c: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: `c` is a correctly sized, initialized PROCESS_MEMORY_COUNTERS
        // and the handle is the current-process pseudo-handle.
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        if ok == 0 {
            return None;
        }
        Some(c.PeakWorkingSetSize as u64)
    }

    pub fn memory_status() -> Option<MemoryStatus> {
        let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: `m` is a correctly sized, initialized MEMORYSTATUSEX.
        let ok = unsafe { GlobalMemoryStatusEx(&mut m) };
        if ok == 0 {
            return None;
        }
        Some(MemoryStatus {
            total: m.ullTotalPhys,
            available: m.ullAvailPhys,
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{MemoryStatus, PriorityMode};
    use std::fs::File;

    pub fn apply_process_policy(_mode: PriorityMode) {}
    pub fn lower_io_priority(_file: &File) {}
    pub fn memory_status() -> Option<MemoryStatus> {
        None
    }
    /// Byte range used purely as a mutex. It sits far beyond any real
    /// archive, so locking it excludes other writers without blocking reads
    /// of the data - unlike a whole-file lock, which would make our own
    /// extraction threads fail with ERROR_LOCK_VIOLATION.
    const LOCK_SENTINEL: u64 = 0xFFFF_FFFF_FFFF_0000;

    pub fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.Anonymous.Anonymous.Offset = (LOCK_SENTINEL & 0xFFFF_FFFF) as u32;
        ov.Anonymous.Anonymous.OffsetHigh = (LOCK_SENTINEL >> 32) as u32;
        // SAFETY: valid handle, and `ov` is a zeroed OVERLAPPED with only the
        // offset fields set, as LockFileEx requires.
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle() as _,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut ov,
            )
        };
        if ok != 0 {
            return Ok(true);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            // ERROR_LOCK_VIOLATION / ERROR_SHARING_VIOLATION: someone else holds it.
            Some(33) | Some(32) => Ok(false),
            _ => Err(err),
        }
    }

    pub fn peak_memory() -> Option<u64> {
        None
    }
}

/// Apply the process-wide CPU/memory policy. Advisory: failures are ignored.
pub fn apply_process_policy(mode: PriorityMode) {
    imp::apply_process_policy(mode)
}

/// Mark a handle as bulk data traffic so foreground I/O keeps priority.
pub fn lower_io_priority(file: &File) {
    imp::lower_io_priority(file)
}

pub fn memory_status() -> Option<MemoryStatus> {
    imp::memory_status()
}

/// Try to take the archive's writer lock. Returns false when another process
/// already holds it. The lock releases when the file handle closes.
pub fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
    imp::try_lock_exclusive(file)
}

/// Peak working set of this process so far, in bytes. Used to verify that
/// packing really does stay inside its budget.
pub fn peak_memory() -> Option<u64> {
    imp::peak_memory()
}
