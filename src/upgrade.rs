use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use notify::{self, DebouncedEvent, RecursiveMode, Watcher};

const NOTIFY_FILE: &str = "upgrade";

pub fn init() {
    let (tx, rx) = mpsc::channel();
    let watcher = init_watcher(tx).expect("failed to init upgrade watcher");
    thread::spawn(move || {
        watch_notify_file(watcher, rx);
    });
}

fn init_watcher(tx: Sender<DebouncedEvent>) -> notify::Result<impl Watcher> {
    let mut watcher = notify::watcher(tx, Default::default())?;
    watcher.watch(NOTIFY_FILE, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

fn watch_notify_file(_watcher: impl Watcher, rx: Receiver<DebouncedEvent>) {
    for event in rx.iter() {
        debug!("notify: {:?}", event);
        match event {
            DebouncedEvent::NoticeWrite(_) => {
                info!("notify detected");
                super::SHUTDOWN.shutdown(None);
                break;
            }
            _ => {}
        }
    }
}
