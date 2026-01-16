use std::sync::atomic::AtomicU32;
use std::time::Duration;

use crate::Result;

#[cfg(target_os = "linux")]
pub fn futex_wait(addr: &AtomicU32, expected: u32, timeout: Option<Duration>) -> Result<()> {
    use libc::{syscall, timespec, EINTR, EAGAIN, FUTEX_WAIT, SYS_futex};

    let mut ts = timespec { tv_sec: 0, tv_nsec: 0 };
    let ts_ptr = if let Some(timeout) = timeout {
        ts.tv_sec = timeout.as_secs() as libc::time_t;
        ts.tv_nsec = timeout.subsec_nanos() as libc::c_long;
        &ts as *const timespec
    } else {
        std::ptr::null()
    };

    let res = unsafe {
        syscall(
            SYS_futex,
            addr as *const AtomicU32 as *const u32,
            FUTEX_WAIT,
            expected,
            ts_ptr,
            std::ptr::null::<u32>(),
            0,
        )
    };
    if res == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == EAGAIN || code == EINTR => Ok(()),
        _ => Err(crate::Error::Io(err)),
    }
}

#[cfg(target_os = "linux")]
pub fn futex_wake(addr: &AtomicU32) -> Result<()> {
    use libc::{syscall, FUTEX_WAKE, SYS_futex};
    let res = unsafe {
        syscall(
            SYS_futex,
            addr as *const AtomicU32 as *const u32,
            FUTEX_WAKE,
            i32::MAX,
            std::ptr::null::<u32>(),
            std::ptr::null::<u32>(),
            0,
        )
    };
    if res < 0 {
        return Err(crate::Error::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn futex_wait(_addr: &AtomicU32, _expected: u32, timeout: Option<Duration>) -> Result<()> {
    if let Some(timeout) = timeout {
        std::thread::sleep(timeout);
    } else {
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn futex_wake(_addr: &AtomicU32) -> Result<()> {
    Ok(())
}
