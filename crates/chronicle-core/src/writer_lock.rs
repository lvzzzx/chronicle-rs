use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{Error, Result};
use std::os::unix::io::AsRawFd;

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
    let file = OpenOptions::new().read(true).open(path)?;
    lock_owner_alive(&file)
}

#[cfg(target_os = "linux")]
fn lock_owner_alive(file: &File) -> Result<bool> {
    let (pid, start_time, _) = read_lock_record(file)?;
    if pid == 0 {
        return Ok(false);
    }
    let proc_start = proc_start_time(pid)?;
    Ok(proc_start == start_time)
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
    for _ in 0..20 {
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
fn lock_owner_alive(_file: &File) -> Result<bool> {
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
fn lock_identity() -> Result<(u32, u64)> {
    let pid = std::process::id();
    Ok((pid, 0))
}

#[cfg(not(target_os = "linux"))]
fn read_lock_record(_file: &File) -> Result<(u32, u64, u64)> {
    Ok((0, 0, 0))
}
