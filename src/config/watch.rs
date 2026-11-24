use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::ErrorKind;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

use super::{FileConfig, Settings, default_config_paths};
use crate::cli::Cli;

pub(crate) struct Config {
    paths: Vec<PathBuf>,
    required: bool,
    watcher: Option<Watcher>,
    reload_at: Option<Instant>,
}

impl Config {
    pub(crate) fn new(cli: Cli) -> Result<Self, String> {
        let required = cli.config.is_some();
        let paths = if cli.no_config {
            Vec::new()
        } else {
            cli.config
                .map_or_else(default_config_paths, |path| vec![path])
        }
        .into_iter()
        .map(std::path::absolute)
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| format!("could not resolve config paths: {error}"))?;
        Ok(Self {
            paths,
            required,
            watcher: None,
            reload_at: None,
        })
    }

    pub(crate) fn load(&self) -> Result<Settings, String> {
        for path in &self.paths {
            let contents = match std::fs::read_to_string(path) {
                Ok(contents) => contents,
                Err(error) if !self.required && error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("could not read {}: {error}", path.display())),
            };
            let mut file: FileConfig = toml::from_str(&contents)
                .map_err(|error| format!("invalid {}: {error}", path.display()))?;
            file.directory = path.parent().unwrap_or(Path::new("")).to_owned();
            return Settings::resolve(file);
        }
        Settings::resolve(FileConfig::default())
    }

    pub(crate) fn watch(&mut self) {
        if self.paths.is_empty() {
            return;
        }
        match Watcher::new(&self.paths) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(error) => log::warn!("could not watch configuration: {error}"),
        }
    }

    pub(crate) fn fd(&self) -> Option<&OwnedFd> {
        self.watcher.as_ref().map(|watcher| &watcher.fd)
    }

    pub(crate) fn changed(&mut self) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        match watcher.changed() {
            Ok(true) => self.reload_at = Some(Instant::now() + Duration::from_millis(100)),
            Ok(false) => {}
            Err(error) => {
                log::warn!("could not read config changes: {error}");
                // Reloading rebuilds the watcher and catches missed changes.
                self.watcher = None;
                self.reload_at = Some(Instant::now() + Duration::from_secs(1));
            }
        }
    }

    pub(crate) fn next_wakeup(&self) -> Option<Instant> {
        self.reload_at
    }

    pub(crate) fn reload(&mut self, now: Instant) -> Option<Result<Settings, String>> {
        if self.reload_at.is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.reload_at = None;
        // Reattach before reading, including after atomic saves and symlink changes.
        self.watch();
        Some(self.load())
    }
}

struct Watcher {
    fd: OwnedFd,
    entries: BTreeMap<i32, BTreeSet<OsString>>,
}

impl Watcher {
    fn new(paths: &[PathBuf]) -> rustix::io::Result<Self> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        let mut entries: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        let mut pending: Vec<_> = paths.iter().cloned().map(|path| (path, 0)).collect();
        let mut visited = BTreeSet::new();
        while let Some((path, depth)) = pending.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            let ancestors: Vec<_> = path.ancestors().collect();
            // Observe replacements before resolving descendants through symlinks.
            for (index, &ancestor) in ancestors.iter().enumerate().rev() {
                // Directories skip content events, so unrelated writes stay silent. Their
                // ATTRIB events catch permission fixes on a config that could not be watched.
                let flags = WatchFlags::DELETE_SELF
                    | WatchFlags::MOVE_SELF
                    | WatchFlags::ATTRIB
                    | if index == 0 {
                        WatchFlags::CLOSE_WRITE | WatchFlags::MODIFY
                    } else {
                        WatchFlags::CREATE
                            | WatchFlags::DELETE
                            | WatchFlags::MOVED_FROM
                            | WatchFlags::MOVED_TO
                    };
                match inotify::add_watch(&fd, ancestor, flags) {
                    Ok(wd) => {
                        let names = entries.entry(wd).or_default();
                        if let Some(name) = index
                            .checked_sub(1)
                            .and_then(|index| ancestors[index].file_name())
                        {
                            names.insert(OsString::from(name));
                        }
                    }
                    Err(
                        rustix::io::Errno::NOENT
                        | rustix::io::Errno::NOTDIR
                        | rustix::io::Errno::ACCESS
                        | rustix::io::Errno::LOOP,
                    ) => {}
                    Err(error) => return Err(error),
                }
                // Watch every symlink's target path too, including intermediate
                // links such as /etc/static used by NixOS activation.
                if depth < 40
                    && let Ok(target) = std::fs::read_link(ancestor)
                {
                    let target = if target.is_absolute() {
                        target
                    } else {
                        ancestor.parent().unwrap_or(ancestor).join(target)
                    };
                    let suffix = path.strip_prefix(ancestor).unwrap();
                    let target = if suffix.as_os_str().is_empty() {
                        target
                    } else {
                        target.join(suffix)
                    };
                    pending.push((target, depth + 1));
                }
            }
        }
        Ok(Self { fd, entries })
    }

    fn changed(&self) -> rustix::io::Result<bool> {
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buffer);
        let mut changed = false;
        loop {
            match reader.next() {
                Ok(event) => {
                    changed |= event.events().contains(ReadFlags::QUEUE_OVERFLOW)
                        || self.entries.get(&event.wd()).is_some_and(|names| {
                            event.file_name().is_none_or(|name| {
                                names
                                    .iter()
                                    .any(|expected| expected.as_bytes() == name.to_bytes())
                            })
                        });
                }
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::AGAIN) => return Ok(changed),
                Err(error) => return Err(error),
            }
        }
    }
}
