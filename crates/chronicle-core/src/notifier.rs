use crate::Result;
#[cfg(target_os = "linux")]
use crate::Error;

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::HashMap;
    use std::ffi::CString;
    use std::fs;
    use std::io::Read;
    use std::mem;
    use std::os::unix::io::RawFd;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use libc::{
        c_int, inotify_add_watch, inotify_event, inotify_init1, poll, pollfd, read, write, IN_CLOEXEC,
        IN_CREATE, IN_DELETE, IN_MOVED_FROM, IN_MOVED_TO, O_CLOEXEC, O_RDWR, POLLIN,
    };

    use super::{Error, Result};

    const EFD_SUFFIX: &str = ".efd";

    pub struct ReaderNotifier {
        fd: RawFd,
        efd_path: PathBuf,
    }

    impl ReaderNotifier {
        pub fn new(readers_dir: &Path, name: &str) -> Result<Self> {
            fs::create_dir_all(readers_dir)?;
            let efd_path = readers_dir.join(format!("{name}{EFD_SUFFIX}"));
            let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
            if fd < 0 {
                return Err(Error::Io(std::io::Error::last_os_error()));
            }
            write_efd_record(&efd_path, std::process::id(), fd)?;
            Ok(Self { fd, efd_path })
        }

        pub fn wait(&self) -> Result<()> {
            let mut pfd = pollfd {
                fd: self.fd,
                events: POLLIN,
                revents: 0,
            };
            let res = unsafe { poll(&mut pfd, 1, -1) };
            if res < 0 {
                return Err(Error::Io(std::io::Error::last_os_error()));
            }
            let mut buf: u64 = 0;
            let n = unsafe { read(self.fd, &mut buf as *mut u64 as *mut _, mem::size_of::<u64>()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(());
                }
                return Err(Error::Io(err));
            }
            Ok(())
        }
    }

    impl Drop for ReaderNotifier {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.efd_path);
            unsafe {
                libc::close(self.fd);
            }
        }
    }

    pub struct WriterNotifier {
        readers: Arc<Mutex<HashMap<String, RawFd>>>,
        inotify: InotifyWorker,
    }

    impl WriterNotifier {
        pub fn new(readers_dir: &Path) -> Result<Self> {
            fs::create_dir_all(readers_dir)?;
            let readers = Arc::new(Mutex::new(HashMap::new()));
            scan_existing(readers_dir, &readers)?;
            let inotify = InotifyWorker::new(readers_dir.to_path_buf(), Arc::clone(&readers))?;
            Ok(Self { readers, inotify })
        }

        pub fn notify_all(&self) -> Result<()> {
            let entries = {
                let guard = self
                    .readers
                    .lock()
                    .map_err(|_| Error::Corrupt("reader notifier lock poisoned"))?;
                guard
                    .iter()
                    .map(|(name, fd)| (name.clone(), *fd))
                    .collect::<Vec<_>>()
            };
            let mut value: u64 = 1;
            let mut stale = Vec::new();
            for (name, fd) in entries {
                let res = unsafe { write(fd, &mut value as *mut u64 as *mut _, mem::size_of::<u64>()) };
                if res < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::EAGAIN) {
                        stale.push(name);
                    }
                }
            }
            if !stale.is_empty() {
                if let Ok(mut guard) = self.readers.lock() {
                    for name in stale {
                        if let Some(fd) = guard.remove(&name) {
                            unsafe {
                                libc::close(fd);
                            }
                        }
                    }
                }
            }
            Ok(())
        }
    }

    impl Drop for WriterNotifier {
        fn drop(&mut self) {
            self.inotify.stop();
            if let Ok(mut guard) = self.readers.lock() {
                for (_name, fd) in guard.drain() {
                    unsafe {
                        libc::close(fd);
                    }
                }
            }
        }
    }

    fn write_efd_record(path: &Path, pid: u32, fd: RawFd) -> Result<()> {
        let tmp_path = path.with_extension("efd.tmp");
        let contents = format!("{pid} {fd}\n");
        fs::write(&tmp_path, contents)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn scan_existing(readers_dir: &Path, readers: &Arc<Mutex<HashMap<String, RawFd>>>) -> Result<()> {
        for entry in fs::read_dir(readers_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("efd") {
                continue;
            }
            if let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) {
                let _ = add_reader(name, &path, readers);
            }
        }
        Ok(())
    }

    struct InotifyWorker {
        fd: Option<RawFd>,
        shutdown: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl InotifyWorker {
        fn new(readers_dir: PathBuf, readers: Arc<Mutex<HashMap<String, RawFd>>>) -> Result<Self> {
        let dir_cstr = CString::new(readers_dir.to_string_lossy().as_bytes())
            .map_err(|_| Error::Unsupported("readers path contains NUL"))?;
        let inotify_fd = unsafe { inotify_init1(IN_CLOEXEC) };
        if inotify_fd < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        let mask = IN_CREATE | IN_DELETE | IN_MOVED_TO | IN_MOVED_FROM;
        let watch = unsafe { inotify_add_watch(inotify_fd, dir_cstr.as_ptr(), mask) };
        if watch < 0 {
            unsafe {
                libc::close(inotify_fd);
            }
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            let mut buffer = vec![0u8; 4096];
            let mut pfd = pollfd {
                fd: inotify_fd,
                events: POLLIN,
                revents: 0,
            };
            let header_size = mem::size_of::<inotify_event>();
            loop {
                if thread_shutdown.load(Ordering::Acquire) {
                    break;
                }
                let res = unsafe { poll(&mut pfd, 1, 100) };
                if res < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    if thread_shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                if res == 0 {
                    continue;
                }
                if (pfd.revents & POLLIN) == 0 {
                    continue;
                }
                let len = unsafe { read(inotify_fd, buffer.as_mut_ptr() as *mut _, buffer.len()) };
                if len <= 0 {
                    if thread_shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    continue;
                }
                let mut offset = 0usize;
                while offset + header_size <= len as usize {
                    let header = &buffer[offset..offset + header_size];
                    let (mask, name_len) = match parse_event_header(header) {
                        Some(header) => header,
                        None => break,
                    };
                    let name_len = name_len as usize;
                    let name_start = offset + header_size;
                    let name_end = name_start.saturating_add(name_len);
                    if name_end > len as usize {
                        break;
                    }
                    let name = if name_len > 0 {
                        let name_bytes = &buffer[name_start..name_end];
                        let nul = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
                        String::from_utf8_lossy(&name_bytes[..nul]).into_owned()
                    } else {
                        String::new()
                    };

                    if name.ends_with(EFD_SUFFIX) {
                        let reader_name = name.trim_end_matches(EFD_SUFFIX);
                        let path = readers_dir.join(&name);
                        if (mask & (IN_CREATE | IN_MOVED_TO)) != 0 {
                            let _ = add_reader(reader_name, &path, &readers);
                        }
                        if (mask & (IN_DELETE | IN_MOVED_FROM)) != 0 {
                            remove_reader(reader_name, &readers);
                        }
                    }

                    offset = name_end;
                }
            }
        });

        Ok(Self {
            fd: Some(inotify_fd),
            shutdown,
            handle: Some(handle),
        })
    }

        fn stop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
            if let Some(fd) = self.fd.take() {
                unsafe {
                    libc::close(fd);
                }
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn parse_event_header(buf: &[u8]) -> Option<(u32, u32)> {
        if buf.len() < mem::size_of::<inotify_event>() {
            return None;
        }
        let mask = u32::from_ne_bytes(buf[4..8].try_into().ok()?);
        let len = u32::from_ne_bytes(buf[12..16].try_into().ok()?);
        Some((mask, len))
    }

    fn add_reader(name: &str, path: &Path, readers: &Arc<Mutex<HashMap<String, RawFd>>>) -> Result<()> {
        let (pid, fd) = parse_efd_record(path)?;
        let new_fd = open_proc_fd(pid, fd)?;
        let mut guard = readers
            .lock()
            .map_err(|_| Error::Corrupt("reader notifier lock poisoned"))?;
        if let Some(old_fd) = guard.insert(name.to_string(), new_fd) {
            unsafe {
                libc::close(old_fd);
            }
        }
        Ok(())
    }

    fn remove_reader(name: &str, readers: &Arc<Mutex<HashMap<String, RawFd>>>) {
        if let Ok(mut guard) = readers.lock() {
            if let Some(fd) = guard.remove(name) {
                unsafe {
                    libc::close(fd);
                }
            }
        }
    }

    fn parse_efd_record(path: &Path) -> Result<(u32, RawFd)> {
        let mut contents = String::new();
        let mut file = fs::File::open(path)?;
        file.read_to_string(&mut contents)?;
        let mut parts = contents.split_whitespace();
        let pid = parts
            .next()
            .ok_or(Error::Corrupt("missing pid in efd record"))?
            .parse::<u32>()
            .map_err(|_| Error::Corrupt("invalid pid in efd record"))?;
        let fd = parts
            .next()
            .ok_or(Error::Corrupt("missing fd in efd record"))?
            .parse::<c_int>()
            .map_err(|_| Error::Corrupt("invalid fd in efd record"))?;
        Ok((pid, fd))
    }

    fn open_proc_fd(pid: u32, fd: RawFd) -> Result<RawFd> {
        let path = format!("/proc/{pid}/fd/{fd}");
        let cpath = CString::new(path).map_err(|_| Error::Unsupported("proc path contains NUL"))?;
        let new_fd = unsafe { libc::open(cpath.as_ptr(), O_RDWR | O_CLOEXEC) };
        if new_fd < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(new_fd)
    }

    pub(super) use ReaderNotifier as ReaderNotifierImpl;
    pub(super) use WriterNotifier as WriterNotifierImpl;
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::path::Path;
    use std::time::Duration;

    use super::Result;

    pub struct ReaderNotifier {
        sleep: Duration,
    }

    impl ReaderNotifier {
        pub fn new(_readers_dir: &Path, _name: &str) -> Result<Self> {
            Ok(Self {
                sleep: Duration::from_millis(1),
            })
        }

        pub fn wait(&self) -> Result<()> {
            std::thread::sleep(self.sleep);
            Ok(())
        }
    }

    pub struct WriterNotifier;

    impl WriterNotifier {
        pub fn new(_readers_dir: &Path) -> Result<Self> {
            Ok(Self)
        }

        pub fn notify_all(&self) -> Result<()> {
            Ok(())
        }
    }

    pub(super) use ReaderNotifier as ReaderNotifierImpl;
    pub(super) use WriterNotifier as WriterNotifierImpl;
}

pub struct WriterNotifier {
    inner: platform::WriterNotifierImpl,
}

pub struct ReaderNotifier {
    inner: platform::ReaderNotifierImpl,
}

impl WriterNotifier {
    pub fn new(readers_dir: &std::path::Path) -> Result<Self> {
        Ok(Self {
            inner: platform::WriterNotifierImpl::new(readers_dir)?,
        })
    }

    pub fn notify_all(&self) -> Result<()> {
        self.inner.notify_all()
    }
}

impl ReaderNotifier {
    pub fn new(readers_dir: &std::path::Path, name: &str) -> Result<Self> {
        Ok(Self {
            inner: platform::ReaderNotifierImpl::new(readers_dir, name)?,
        })
    }

    pub fn wait(&self) -> Result<()> {
        self.inner.wait()
    }
}
