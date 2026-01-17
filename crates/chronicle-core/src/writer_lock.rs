use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::Path;

use crate::{Error, Result};
use std::os::unix::io::AsRawFd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterLockInfo {
    pub pid: u32,
    pub start_time: u64,
    pub epoch: u64,
}

pub(crate) fn try_lock(file: &File) -> Result<bool> {
    let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if res == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(false);
    }
    Err(Error::Io(err))
}

pub(crate) fn write_lock_record(file: &File, writer_epoch: u64) -> Result<()> {
    let (pid, start_time) = lock_identity()?;
    let record = format!("{pid} {start_time} {writer_epoch}\n");
    let mut handle = file.try_clone()?;
    handle.set_len(0)?;
    handle.seek(SeekFrom::Start(0))?;
    handle.write_all(record.as_bytes())?;
    handle.sync_all()?;
    Ok(())
}

pub(crate) fn writer_alive(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let info = match read_lock_info(path)? {
        Some(info) => info,
        None => return Ok(false),
    };
    lock_owner_alive(&info)
}

pub fn read_lock_info(path: &Path) -> Result<Option<WriterLockInfo>> {
    if !path.exists() {
        return Ok(None);
    }
    let file = OpenOptions::new().read(true).open(path)?;
    let (pid, start_time, epoch) = read_lock_record(&file)?;
    let info = WriterLockInfo {
        pid,
        start_time,
        epoch,
    };
    #[cfg(target_os = "linux")]
    {
        if info.pid == 0 && info.start_time == 0 && info.epoch == 0 {
            return Ok(None);
        }
    }
    Ok(Some(info))
}

#[cfg(target_os = "linux")]
pub fn lock_owner_alive(info: &WriterLockInfo) -> Result<bool> {
    if info.pid == 0 {
        return Ok(false);
    }
    let proc_start = proc_start_time(info.pid)?;
    Ok(proc_start == info.start_time)
}

#[cfg(not(target_os = "linux"))]
pub fn lock_owner_alive(_info: &WriterLockInfo) -> Result<bool> {
    Ok(true)
}

#[cfg(target_os = "linux")]
fn lock_identity() -> Result<(u32, u64)> {
    let pid = std::process::id();
    let start_time = proc_start_time(pid)?;
    Ok((pid, start_time))
}

#[cfg(target_os = "linux")]
fn proc_start_time(pid: u32) -> Result<u64> {
    let path = format!("/proc/{pid}/stat");
    let mut contents = String::new();
    File::open(&path)?.read_to_string(&mut contents)?;
    let end = contents.rfind(')').ok_or(Error::CorruptMetadata("stat parse"))?;
    let after = &contents[end + 1..];
    let mut fields = after.split_whitespace();
    for _ in 0..19 {
        fields.next();
    }
    let start = fields
        .next()
        .ok_or(Error::CorruptMetadata("stat missing starttime"))?;
    let start_time = start
        .parse::<u64>()
        .map_err(|_| Error::CorruptMetadata("stat starttime invalid"))?;
    Ok(start_time)
}

#[cfg(target_os = "linux")]
fn read_lock_record(file: &File) -> Result<(u32, u64, u64)> {
    let mut contents = String::new();
    let mut clone = file.try_clone()?;
    clone.seek(SeekFrom::Start(0))?;
    clone.read_to_string(&mut contents)?;
    let mut parts = contents.split_whitespace();
    let pid = parts
        .next()
        .unwrap_or("0")
        .parse::<u32>()
        .unwrap_or(0);
    let start_time = parts
        .next()
        .unwrap_or("0")
        .parse::<u64>()
        .unwrap_or(0);
    let epoch = parts
        .next()
        .unwrap_or("0")
        .parse::<u64>()
        .unwrap_or(0);
    Ok((pid, start_time, epoch))
}

#[cfg(not(target_os = "linux"))]
fn lock_identity() -> Result<(u32, u64)> {
    let pid = std::process::id();
    Ok((pid, 0))
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn read_lock_record(_file: &File) -> Result<(u32, u64, u64)> {
    Ok((0, 0, 0))
}
