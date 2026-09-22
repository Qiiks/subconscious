#![deny(unsafe_code)]

use std::time::Duration;

/// Read elapsed time since boot, including host sleep.
///
/// Linux CLOCK_BOOTTIME includes suspend, while its CLOCK_MONOTONIC does not.
/// Darwin CLOCK_MONOTONIC includes suspend; `std::time::Instant` instead uses
/// mach_absolute_time (CLOCK_UPTIME_RAW), which stops during sleep. Windows
/// GetTickCount64 includes sleep, unlike QueryUnbiasedInterruptTime.
#[cfg(unix)]
pub fn suspend_inclusive_now() -> Duration {
    use rustix::time::{clock_gettime, ClockId};
    #[cfg(target_os = "linux")]
    let clock = ClockId::Boottime;
    #[cfg(not(target_os = "linux"))]
    let clock = ClockId::Monotonic;
    let time = clock_gettime(clock);
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

/// Read elapsed time since boot, including host sleep.
///
/// GetTickCount64 includes sleep; QueryUnbiasedInterruptTime does not.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn suspend_inclusive_now() -> Duration {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    // SAFETY: GetTickCount64 takes no arguments, has no preconditions, and cannot fail.
    Duration::from_millis(unsafe { GetTickCount64() })
}

#[cfg(test)]
mod tests {
    #[test]
    fn readings_do_not_go_backwards() {
        assert!(super::suspend_inclusive_now() <= super::suspend_inclusive_now());
    }
}
