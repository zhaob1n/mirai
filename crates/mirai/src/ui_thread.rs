// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Asks the kernel to clock the CPU up while the GTK thread runs.
//!
//! Where the kernel picks a core's frequency from how busy its tasks look — schedutil, as
//! with intel_pstate in passive mode or acpi-cpufreq — the GTK thread looks idle: it works
//! a millisecond or two a frame and sleeps the rest. Measured on a 12th-generation Intel
//! running schedutil, its frames ran at 0.65–0.97 GHz of a 4.9 GHz core, so work worth 1–2
//! ms at full clock took 5–17 ms, and that, not the work, put most of the remaining frames
//! over a 160 Hz budget. A utilization clamp (`uclamp.min`) has schedutil treat the thread
//! as at least [`CLAMP`] busy while it runs; asleep it costs nothing. Setting it needs no
//! privilege. A kernel built without `CONFIG_UCLAMP_TASK` refuses it, and a frequency
//! driver that does not ask the scheduler (intel_pstate with HWP, amd-pstate in active
//! mode) ramps up by itself; either way nothing changes.
//!
//! The clamp is the GTK thread's alone: reset-on-fork keeps every thread and process it
//! starts — the runtime's workers, KataGo — from inheriting it.

/// Out of 1024: at least half, which schedutil turns into about 60 % of the top clock.
const CLAMP: u32 = 512;

/// Applies the clamp to the calling thread, which must be the GTK thread. Failure only
/// means the kernel cannot clamp; it is logged and otherwise ignored.
pub(crate) fn request_clock_floor() {
    #[cfg(target_os = "linux")]
    if let Err(error) = linux::clamp_util_min(CLAMP) {
        tracing::debug!(%error, "the kernel declined a utilization clamp for the GTK thread");
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::c_long;
    use std::io;

    /// `struct sched_attr` from `<linux/sched/types.h>`, up to the utilization clamps.
    #[repr(C)]
    struct SchedAttr {
        size: u32,
        sched_policy: u32,
        sched_flags: u64,
        sched_nice: i32,
        sched_priority: u32,
        sched_runtime: u64,
        sched_deadline: u64,
        sched_period: u64,
        sched_util_min: u32,
        sched_util_max: u32,
    }

    const SCHED_OTHER: u32 = 0;
    const SCHED_FLAG_RESET_ON_FORK: u64 = 0x01;
    const SCHED_FLAG_KEEP_PARAMS: u64 = 0x10;
    const SCHED_FLAG_UTIL_CLAMP_MIN: u64 = 0x20;

    // glibc has no wrapper for sched_setattr, and the number is per architecture.
    #[cfg(target_arch = "x86_64")]
    const SYS_SCHED_SETATTR: c_long = 314;
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    ))]
    const SYS_SCHED_SETATTR: c_long = 274;

    unsafe extern "C" {
        fn syscall(number: c_long, ...) -> c_long;
    }

    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    ))]
    pub(super) fn clamp_util_min(util_min: u32) -> io::Result<()> {
        // SCHED_OTHER rather than SCHED_FLAG_KEEP_POLICY: keeping the policy also keeps
        // the old reset-on-fork, which is off. The GTK thread is SCHED_OTHER anyway, and
        // KEEP_PARAMS leaves its nice value alone.
        let attr = SchedAttr {
            size: std::mem::size_of::<SchedAttr>() as u32,
            sched_policy: SCHED_OTHER,
            sched_flags: SCHED_FLAG_RESET_ON_FORK
                | SCHED_FLAG_KEEP_PARAMS
                | SCHED_FLAG_UTIL_CLAMP_MIN,
            sched_nice: 0,
            sched_priority: 0,
            sched_runtime: 0,
            sched_deadline: 0,
            sched_period: 0,
            sched_util_min: util_min,
            sched_util_max: 0,
        };
        // SAFETY: `attr` is a valid `sched_attr` whose `size` covers exactly the fields
        // written; pid 0 is the calling thread; the kernel only reads it.
        let result = unsafe {
            syscall(
                SYS_SCHED_SETATTR,
                0 as c_long,
                &attr as *const SchedAttr,
                0 as c_long,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    )))]
    pub(super) fn clamp_util_min(_util_min: u32) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}
