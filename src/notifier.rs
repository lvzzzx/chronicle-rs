use crate::Result;
#[cfg(target_os = "linux")]
use crate::Error;

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::HashMap;
    use std::ffi::{CStr, CString};
    use std::fs;
    use std::io::Read;
    use std::mem;
    use std::os::unix::io::RawFd;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;
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
    }

    impl WriterNotifier {
        pub fn new(readers_dir: &Path) -> Result<Self> {
            fs::create_dir_all(readers_dir)?;
            let readers = Arc::new(Mutex::new(HashMap::new()));
            scan_existing(readers_dir, &readers)?;
            spawn_inotify(readers_dir.to_path_buf(), Arc::clone(&readers))?;
            Ok(Self { readers })
        }

        pub fn notify_all(&self) -> Result<()> {
            let fds = {
                let guard = self
                    .readers
                    .lock()
                    .map_err(|_| Error::Corrupt("reader notifier lock poisoned"))?;
                guard.values().copied().collect::<Vec<_>>()
            };
            let mut value: u64 = 1;
            for fd in fds {
                let res = unsafe { write(fd, &mut value as *mut u64 as *mut _, mem::size_of::<u64>()) };
                if res < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EAGAIN) {
                        continue;
                    }
                    return Err(Error::Io(err));
                }
            }
            Ok(())
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

    fn spawn_inotify(readers_dir: PathBuf, readers: Arc<Mutex<HashMap<String, RawFd>>>) -> Result<()> {
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

        thread::spawn(move || {
            let mut buffer = vec![0u8; 4096];
            loop {
                let len = unsafe { read(inotify_fd, buffer.as_mut_ptr() as *mut _, buffer.len()) };
                if len < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                let mut offset = 0usize;
                while offset < len as usize {
                    let event_ptr = unsafe { buffer.as_ptr().add(offset) as *const inotify_event };
                    let event = unsafe { &*event_ptr };
                    let name_len = event.len as usize;
                    let name = if name_len > 0 {
                        let name_ptr = unsafe { buffer.as_ptr().add(offset + mem::size_of::<inotify_event>()) };
                        unsafe { CStr::from_ptr(name_ptr as *const i8) }
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        String::new()
                    };

                    if name.ends_with(EFD_SUFFIX) {
                        let reader_name = name.trim_end_matches(EFD_SUFFIX);
                        let path = readers_dir.join(&name);
                        if (event.mask & (IN_CREATE | IN_MOVED_TO)) != 0 {
                            let _ = add_reader(reader_name, &path, &readers);
                        }
                        if (event.mask & (IN_DELETE | IN_MOVED_FROM)) != 0 {
                            remove_reader(reader_name, &readers);
                        }
                    }

                    let event_size = mem::size_of::<inotify_event>() + name_len;
                    offset += event_size;
                }
            }
        });

        Ok(())
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
