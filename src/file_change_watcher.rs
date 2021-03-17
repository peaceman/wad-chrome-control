use std::path::{Path, PathBuf};
use std::result::Result as StdResult;
use std::time::Duration;
use std::{env, error::Error as StdError};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

type Result<T> = StdResult<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Generic(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Watch not found")]
    WatchNotFound,
}

#[derive(Debug)]
pub struct ChangeEvent(PathBuf);

pub struct Watch {
    cookie: Cookie,
    rx: UnboundedReceiver<ChangeEvent>,
}

impl Watch {
    pub fn channel(&mut self) -> &mut UnboundedReceiver<ChangeEvent> {
        &mut self.rx
    }
}

pub trait Watcher: Sized {
    fn new(delay: Duration) -> Result<Self>;

    fn watch(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Watch>;

    fn unwatch(&self, watch: Watch) -> Result<()>;
}

pub fn watcher(delay: Duration) -> Result<impl Watcher> {
    notify::NotifyWatcher::new(delay)
}

#[derive(Debug, Hash, PartialEq, Eq, Copy, Clone)]
pub struct Cookie(usize);

mod notify {
    use notify::Watcher as _;
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

    use super::{ChangeEvent, Cookie, Result, Watch, Watcher, absolute_path_buf, parent_path};

    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;
    use std::{collections::hash_map::Entry, env};
    use std::{
        collections::HashMap,
        sync::mpsc::{channel, Receiver, Sender},
    };

    enum EventLoopMsg {
        AddWatch(
            PathBuf,
            Sender<Result<Watch>>,
        ),
        RemoveWatch(Cookie, Sender<Result<()>>),
        Shutdown,
    }

    pub struct NotifyWatcher(Mutex<Sender<EventLoopMsg>>);

    impl Watcher for NotifyWatcher {
        fn new(delay: Duration) -> Result<Self> {
            let (event_loop_tx, event_loop_rx) = channel();

            let event_loop = EventLoop::new(event_loop_rx, delay)?;
            event_loop.run();

            Ok(Self(Mutex::new(event_loop_tx)))
        }

        fn watch(
            &self,
            path: impl AsRef<Path>,
        ) -> Result<Watch> {
            let pb = absolute_path_buf(path)?;
            let (r_tx, r_rx) = channel();

            let msg = EventLoopMsg::AddWatch(pb, r_tx);

            self.0.lock().unwrap().send(msg).unwrap();
            r_rx.recv().unwrap()
        }

        fn unwatch(&self, watch: Watch) -> Result<()> {
            let (r_tx, r_rx) = channel();

            let msg = EventLoopMsg::RemoveWatch(watch.cookie, r_tx);

            self.0.lock().unwrap().send(msg).unwrap();
            r_rx.recv().unwrap()
        }
    }

    impl Drop for NotifyWatcher {
        fn drop(&mut self) {
            let msg = EventLoopMsg::Shutdown;
            self.0.lock().unwrap().send(msg).unwrap();
        }
    }

    struct EventLoop {
        event_loop_rx: Receiver<EventLoopMsg>,
        watcher_rx: Receiver<notify::DebouncedEvent>,
        watcher: notify::RecommendedWatcher,
        watches: HashMap<PathBuf, HashMap<Cookie, UnboundedSender<ChangeEvent>>>,
        paths: HashMap<Cookie, PathBuf>,
        parent_folders: HashMap<PathBuf, usize>,
        running: bool,
        cookie_counter: usize,
    }

    impl EventLoop {
        fn new(event_loop_rx: Receiver<EventLoopMsg>, delay: Duration) -> Result<Self> {
            let (watcher_tx, watcher_rx) = channel();
            let watcher = notify::watcher(watcher_tx, delay)?;

            let event_loop = Self {
                event_loop_rx,
                watcher_rx,
                watcher,
                watches: HashMap::new(),
                paths: HashMap::new(),
                parent_folders: HashMap::new(),
                running: true,
                cookie_counter: 0,
            };

            Ok(event_loop)
        }

        fn run(self) {
            thread::spawn(|| self.event_loop_thread());
        }

        fn event_loop_thread(mut self) {
            loop {
                while let Ok(msg) = self.event_loop_rx.try_recv() {
                    self.handle_event_loop_message(msg);
                }

                while let Ok(event) = self.watcher_rx.try_recv() {
                    self.handle_watcher_event(event);
                }

                if !self.running {
                    break;
                }

                thread::sleep(Duration::from_millis(100));
            }
        }

        fn handle_event_loop_message(&mut self, msg: EventLoopMsg) {
            match msg {
                EventLoopMsg::AddWatch(path, tx) => {
                    tx.send(self.add_watch(path));
                }
                EventLoopMsg::RemoveWatch(cookie, tx) => {
                    tx.send(self.remove_watch(cookie));
                }
                EventLoopMsg::Shutdown => {
                    self.running = false;
                }
            }
        }

        fn handle_watcher_event(&mut self, event: notify::DebouncedEvent) {
            match event {
                notify::DebouncedEvent::Create(path)
                | notify::DebouncedEvent::Write(path)
                | notify::DebouncedEvent::Remove(path)
                | notify::DebouncedEvent::Rename(_, path) => {
                    self.emit_change_event(path);
                }
                _ => {}
            }
        }

        fn emit_change_event(&mut self, path: PathBuf) {
            if let Some(watches) = self.watches.get(&path) {
                let failed_watches = watches
                    .iter()
                    .map(|(cookie, watch_tx)| (cookie, watch_tx.send(ChangeEvent(path.clone()))))
                    .filter(|(_, send_result)| send_result.is_err())
                    .map(|(cookie, _)| *cookie)
                    .collect::<Vec<_>>();

                for cookie in failed_watches {
                    self.remove_watch(cookie);
                }
            }
        }

        fn add_watch(
            &mut self,
            path: PathBuf,
        ) -> Result<Watch> {
            let parent_path = parent_path(&path)?;

            match self.parent_folders.entry(parent_path.to_owned()) {
                Entry::Occupied(mut e) => {
                    *e.get_mut() += 1;
                }
                Entry::Vacant(e) => {
                    self.watcher
                        .watch(parent_path, notify::RecursiveMode::NonRecursive)?;
                    e.insert(1);
                }
            }

            let cookie = self.new_cookie();
            let watch_list = self
                .watches
                .entry(path.to_owned())
                .or_insert_with(HashMap::new);

            let (watch_tx, watch_rx) = tokio::sync::mpsc::unbounded_channel();
            watch_list.insert(cookie, watch_tx);

            self.paths.insert(cookie, path.to_owned());

            Ok(Watch {
                cookie,
                rx: watch_rx,
            })
        }

        fn remove_watch(&mut self, cookie: Cookie) -> Result<()> {
            let path = self
                .paths
                .get(&cookie)
                .ok_or(super::Error::WatchNotFound)?;

            // if the path was added, this can't fail
            let parent_path = parent_path(&path).unwrap();

            // if this was the last watch on the parent folder, the watch will be removed
            if self.parent_folders[parent_path] - 1 == 0 {
                self.watcher.unwatch(parent_path)?;
                self.parent_folders.remove(parent_path);
            }

            let path_watches = self.watches.get_mut(path).unwrap();
            path_watches.remove(&cookie);

            self.paths.remove(&cookie);

            Ok(())
        }

        fn new_cookie(&mut self) -> Cookie {
            let cookie = Cookie(self.cookie_counter);
            self.cookie_counter += 1;

            cookie
        }
    }

    impl From<notify::Error> for super::Error {
        fn from(error: notify::Error) -> Self {
            match error {
                notify::Error::Generic(s) => Self::Generic(s),
                notify::Error::Io(e) => Self::Io(e),
                notify::Error::PathNotFound => Self::Generic(String::from("Path not found")),
                notify::Error::WatchNotFound => Self::Generic(String::from("Watch not found")),
            }
        }
    }
}

fn parent_path(path: &Path) -> Result<&Path> {
    let parent_path = path.parent().ok_or_else(|| {
        Error::Generic(format!(
            "Failed to determine parent path of `{}`",
            path.display()
        ))
    })?;

    Ok(parent_path)
}

fn absolute_path_buf(path: impl AsRef<Path>) -> Result<PathBuf> {
    let pb = if path.as_ref().is_absolute() {
        path.as_ref().to_owned()
    } else {
        env::current_dir()?.join(path)
    };

    Ok(pb)
}
