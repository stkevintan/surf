//! Real-time scheduling for audio worker threads.
//!
//! A render thread that the scheduler can park for several milliseconds forces
//! a large queue cushion, and that cushion is latency the player hears. Call
//! [`promote_audio_thread`] from the thread itself; keep the returned guard
//! alive for as long as the thread renders.
//!
//! - Windows: join the MMCSS "Pro Audio" task (what DAWs and WASAPI's own engine
//!   use) and raise the timer resolution to 1 ms so `sleep(1ms)` means ~1 ms.
//! - macOS: a Mach time-constraint policy, the same class Core Audio's I/O
//!   threads run in.
//! - Linux: no-op for now. `SCHED_FIFO` needs rtkit or a privileged user.

use std::time::Duration;

/// Undoes the promotion on drop.
pub struct RealtimeGuard {
    #[cfg(windows)]
    mmcss: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(windows)]
    timer_period: bool,
}

/// Promote the calling thread. `period` is how often it wakes to render.
///
/// Never fails: on error it logs to stderr and leaves default scheduling, since
/// a larger cushion still keeps audio intact, just with more latency.
pub fn promote_audio_thread(period: Duration) -> RealtimeGuard {
    imp::promote(period)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
    use windows::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
    };
    use windows::core::w;

    pub fn promote(_period: Duration) -> RealtimeGuard {
        let mut task_index = 0u32;
        // SAFETY: called from the thread being registered; `task_index` is a live out-param.
        let mmcss = match unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }
        {
            Ok(handle) if !handle.is_invalid() => {
                eprintln!("rt: audio thread joined MMCSS 'Pro Audio'");
                Some(handle)
            }
            Ok(_) => None,
            Err(err) => {
                eprintln!("rt: MMCSS registration failed ({err}); using default scheduling");
                None
            }
        };
        // SAFETY: plain system call; balanced in Drop.
        let timer_period = unsafe { timeBeginPeriod(1) } == 0;
        if !timer_period {
            eprintln!("rt: timeBeginPeriod(1) failed; sleeps may be coarse");
        }
        RealtimeGuard {
            mmcss,
            timer_period,
        }
    }

    impl Drop for RealtimeGuard {
        fn drop(&mut self) {
            if self.timer_period {
                // SAFETY: balances the successful timeBeginPeriod(1).
                unsafe {
                    timeEndPeriod(1);
                }
            }
            if let Some(handle) = self.mmcss.take() {
                // SAFETY: handle came from AvSetMmThreadCharacteristicsW on this thread.
                unsafe {
                    let _ = AvRevertMmThreadCharacteristics(handle);
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    // libc marks its copy deprecated in favour of the `mach2` crate; one call
    // does not justify a dependency, so declare it here. ABI is stable.
    #[repr(C)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
    }

    pub fn promote(period: Duration) -> RealtimeGuard {
        let mut timebase = MachTimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: valid out-pointer to a struct with the C layout the call expects.
        unsafe { mach_timebase_info(&mut timebase) };
        if timebase.numer == 0 {
            eprintln!("rt: mach_timebase_info failed; using default scheduling");
            return RealtimeGuard {};
        }
        let to_abs = |d: Duration| -> u32 {
            let ns = d.as_nanos() as u64;
            (ns * timebase.denom as u64 / timebase.numer as u64).min(u32::MAX as u64) as u32
        };
        // Ask for up to half the period of CPU each period, finished within the period.
        let mut policy = libc::thread_time_constraint_policy {
            period: to_abs(period),
            computation: to_abs(period / 2),
            constraint: to_abs(period),
            preemptible: 1,
        };
        // SAFETY: the policy struct matches THREAD_TIME_CONSTRAINT_POLICY and the
        // count is its size in integer_t units, as the API requires.
        let rc = unsafe {
            libc::thread_policy_set(
                libc::pthread_mach_thread_np(libc::pthread_self()),
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                &mut policy as *mut _ as libc::thread_policy_t,
                libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            )
        };
        if rc == 0 {
            eprintln!("rt: audio thread uses a time-constraint policy");
        } else {
            eprintln!("rt: thread_policy_set failed ({rc}); using default scheduling");
        }
        // A time-constraint policy ends with the thread; nothing to undo.
        RealtimeGuard {}
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::*;

    pub fn promote(_period: Duration) -> RealtimeGuard {
        RealtimeGuard {}
    }
}
