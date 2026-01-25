use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::layout::{OrdersLayout, StrategyId};

pub struct RouterDiscovery {
    layout: OrdersLayout,
    initialized: bool,
    known: HashSet<StrategyId>,
    watcher: platform::Watcher,
    last_scan: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    Added {
        strategy: StrategyId,
        orders_out: PathBuf,
    },
    Removed {
        strategy: StrategyId,
    },
}

impl RouterDiscovery {
    pub fn new(layout: OrdersLayout) -> std::io::Result<Self> {
        let orders_root = layout.orders_root();
        fs::create_dir_all(&orders_root)?;
        let watcher = platform::Watcher::new(&orders_root)?;
        Ok(Self {
            layout,
            initialized: false,
            known: HashSet::new(),
            watcher,
            last_scan: None,
        })
    }

    pub fn poll(&mut self) -> std::io::Result<Vec<DiscoveryEvent>> {
        let mut should_scan = false;
        if !self.initialized {
            self.initialized = true;
            should_scan = true;
        }

        if self.watcher.drain()? {
            should_scan = true;
        }

        let now = Instant::now();
        if self
            .last_scan
            .map_or(true, |last| now.duration_since(last) >= RESCAN_INTERVAL)
        {
            should_scan = true;
        }

        if !should_scan {
            return Ok(Vec::new());
        }

        let current = scan_ready_strategies(&self.layout)?;
        let mut events = Vec::new();

        for strategy in current.difference(&self.known) {
            let strategy = strategy.clone();
            let endpoints = self
                .layout
                .strategy_endpoints(&strategy)
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
            events.push(DiscoveryEvent::Added {
                strategy,
                orders_out: endpoints.orders_out,
            });
        }

        for strategy in self.known.difference(&current) {
            events.push(DiscoveryEvent::Removed {
                strategy: strategy.clone(),
            });
        }

        self.known = current;
        self.last_scan = Some(now);
        Ok(events)
    }

    pub fn layout(&self) -> &OrdersLayout {
        &self.layout
    }
}

const RESCAN_INTERVAL: Duration = Duration::from_millis(500);

fn scan_ready_strategies(layout: &OrdersLayout) -> std::io::Result<HashSet<StrategyId>> {
    let root = layout.orders_root();
    fs::create_dir_all(&root)?;
    let mut found = HashSet::new();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    continue;
                }
                return Err(err);
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    continue;
                }
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.is_empty() {
            continue;
        }
        let strategy = StrategyId(name);
        let endpoints = match layout.strategy_endpoints(&strategy) {
            Ok(endpoints) => endpoints,
            Err(_) => continue,
        };
        if is_ready(&endpoints.orders_out) {
            found.insert(strategy);
        }
    }
    Ok(found)
}

fn is_ready(orders_out: &Path) -> bool {
    let ready_path = orders_out.join("READY");
    fs::metadata(ready_path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
mod platform {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::RawFd;
    use std::path::Path;

    use libc::{
        inotify_add_watch, inotify_init1, read, IN_CLOEXEC, IN_CREATE, IN_DELETE, IN_MOVED_FROM,
        IN_MOVED_TO, IN_NONBLOCK,
    };

    pub struct Watcher {
        fd: RawFd,
        buffer: Vec<u8>,
    }

    impl Watcher {
        pub fn new(dir: &Path) -> io::Result<Self> {
            let dir_cstr = CString::new(dir.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "watch path contains NUL")
            })?;
            let fd = unsafe { inotify_init1(IN_CLOEXEC | IN_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let mask = IN_CREATE | IN_DELETE | IN_MOVED_TO | IN_MOVED_FROM;
            let watch = unsafe { inotify_add_watch(fd, dir_cstr.as_ptr(), mask) };
            if watch < 0 {
                unsafe {
                    libc::close(fd);
                }
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                fd,
                buffer: vec![0u8; 4096],
            })
        }

        pub fn drain(&mut self) -> io::Result<bool> {
            let mut saw_event = false;
            loop {
                let len = unsafe {
                    read(
                        self.fd,
                        self.buffer.as_mut_ptr() as *mut _,
                        self.buffer.len(),
                    )
                };
                if len < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::WouldBlock {
                        break;
                    }
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(err);
                }
                if len == 0 {
                    break;
                }
                saw_event = true;
                if (len as usize) < self.buffer.len() {
                    break;
                }
            }
            Ok(saw_event)
        }
    }

    impl Drop for Watcher {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::io;
    use std::path::Path;

    pub struct Watcher;

    impl Watcher {
        pub fn new(_dir: &Path) -> io::Result<Self> {
            Ok(Self)
        }

        pub fn drain(&mut self) -> io::Result<bool> {
            Ok(false)
        }
    }
}
