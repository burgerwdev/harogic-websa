//! Keep capture deadlines ahead of ordinary UI/search work using fair-scheduler
//! weights. No FIFO scheduling, affinity changes, or system configuration.
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::process::Command;
    const CAPTURE_NICE: i32 = -5;
    static REPORTED_OK: AtomicBool = AtomicBool::new(false);
    static REPORTED_ERROR: AtomicBool = AtomicBool::new(false);

    fn grant(tid: u32) {
        if unsafe { libc::getpriority(libc::PRIO_PROCESS, tid) } <= CAPTURE_NICE {
            return;
        }
        // RTKit grants only this user's threads, subject to the system's own
        // limits. The timeout bounds startup when a broker is unavailable.
        let result = Command::new("busctl")
            .args([
                "--system",
                "--timeout=1s",
                "--",
                "call",
                "org.freedesktop.RealtimeKit1",
                "/org/freedesktop/RealtimeKit1",
                "org.freedesktop.RealtimeKit1",
                "MakeThreadHighPriorityWithPID",
                "tti",
                &std::process::id().to_string(),
                &tid.to_string(),
                &CAPTURE_NICE.to_string(),
            ])
            .output();
        let effective = unsafe { libc::getpriority(libc::PRIO_PROCESS, tid) };
        if result.as_ref().is_ok_and(|r| r.status.success()) && effective <= CAPTURE_NICE {
            if !REPORTED_OK.swap(true, Ordering::Relaxed) {
                eprintln!("WebSA capture scheduling: fair scheduler nice={CAPTURE_NICE} via RTKit");
            }
        } else if !REPORTED_ERROR.swap(true, Ordering::Relaxed) {
            let reason = match result {
                Ok(result) => String::from_utf8_lossy(&result.stderr).trim().to_owned(),
                Err(error) => error.to_string(),
            };
            eprintln!("WebSA capture scheduling unavailable; keeping normal priority: {reason}");
        }
    }
    pub(super) fn capture() {
        grant(unsafe { libc::syscall(libc::SYS_gettid) } as u32);
    }
    pub(super) fn normal() {
        // A worker inherits its creator's nice value. Display and fine DSP must
        // not accidentally inherit the capture controller's scheduling weight.
        if unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) } < 0 {
            unsafe {
                libc::setpriority(libc::PRIO_PROCESS, 0, 0);
            }
        }
    }
    pub(super) fn vendor() {
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            return;
        };
        for entry in tasks.flatten() {
            let Ok(name) = std::fs::read_to_string(entry.path().join("comm")) else {
                continue;
            };
            if matches!(
                name.trim(),
                "san90-session" | "iqs-usb-pump" | "sdr-pfb" | "iqs-capture"
            ) || name.trim().starts_with("pfb-")
            {
                if let Ok(tid) = entry.file_name().to_string_lossy().parse() {
                    grant(tid);
                }
            }
        }
    }
}

pub fn capture() {
    #[cfg(target_os = "linux")]
    if ENABLED.load(Ordering::Relaxed) {
        linux::capture();
    }
}
pub fn normal() {
    #[cfg(target_os = "linux")]
    if ENABLED.load(Ordering::Relaxed) {
        linux::normal();
    }
}
/// Capture boundaries also cover SDK workers created lazily by the first read.
pub fn vendor() {
    #[cfg(target_os = "linux")]
    if ENABLED.load(Ordering::Relaxed) {
        linux::vendor();
    }
}
