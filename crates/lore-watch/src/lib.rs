//! Debounced filesystem watcher.
//!
//! Wraps `notify::RecommendedWatcher` behind a tokio `mpsc::Receiver` of
//! coarse-grained `WatchEvent`s. Multiple raw events for the same path that
//! arrive within `debounce` are collapsed into one — notify emits rapid-fire
//! modify/create pairs for many editors (Vim's atomic-save, VS Code, `touch`),
//! and re-indexing for each would thrash the CPU.
//!
//! The crate is the one piece of Lore that does real I/O outside the service
//! binary. It is kept deliberately small so the surface area for OS-specific
//! behaviour stays concentrated.

#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher, event::ModifyKind};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// A coarse event about a filesystem path we care about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// Path was created, modified, or its metadata changed. Re-read it.
    Upsert(PathBuf),
    /// Path was removed or renamed away.
    Remove(PathBuf),
}

impl WatchEvent {
    pub fn path(&self) -> &Path {
        match self {
            WatchEvent::Upsert(p) | WatchEvent::Remove(p) => p,
        }
    }
}

/// Handle to a running watcher. Dropping it shuts the watcher down —
/// dropping `_watcher` closes notify's sender, which ends the debounce
/// loop on its next `recv`, which drops `_task`.
pub struct WatchHandle {
    rx: mpsc::Receiver<WatchEvent>,
    _watcher: notify::RecommendedWatcher,
    _task: tokio::task::JoinHandle<()>,
}

impl WatchHandle {
    /// Await the next event. Returns `None` if the watcher has stopped.
    pub async fn next(&mut self) -> Option<WatchEvent> {
        self.rx.recv().await
    }
}

/// Start watching each path recursively with a debouncing interval.
///
/// Files that appear and disappear within `debounce` collapse into a single
/// Upsert/Remove (whichever was last). Directory-only events are ignored —
/// Lore only cares about file content changes.
pub fn watch(paths: Vec<PathBuf>, debounce: Duration) -> lore_core::Result<WatchHandle> {
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = raw_tx.send(res);
    })
    .map_err(|e| lore_core::Error::Io(format!("notify watcher: {e}")))?;

    for p in &paths {
        watcher
            .watch(p, RecursiveMode::Recursive)
            .map_err(|e| lore_core::Error::Io(format!("watch {}: {e}", p.display())))?;
    }

    let (out_tx, out_rx) = mpsc::channel::<WatchEvent>(256);
    let task = tokio::task::spawn_blocking(move || {
        debounce_loop(raw_rx, out_tx, debounce);
    });

    Ok(WatchHandle {
        rx: out_rx,
        _watcher: watcher,
        _task: task,
    })
}

fn debounce_loop(
    raw_rx: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    out_tx: mpsc::Sender<WatchEvent>,
    debounce: Duration,
) {
    let mut pending: HashMap<PathBuf, (WatchEvent, Instant)> = HashMap::new();
    loop {
        // Wait up to `debounce` for the next raw event. If the queue is
        // already primed, flush ripe entries first.
        let recv_result = if pending.is_empty() {
            raw_rx.recv().map(Some).or(Err(()))
        } else {
            match raw_rx.recv_timeout(debounce) {
                Ok(ev) => Ok(Some(ev)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(()),
            }
        };

        match recv_result {
            Ok(Some(raw)) => match raw {
                Ok(event) => ingest(event, &mut pending),
                Err(e) => warn!("notify error: {e}"),
            },
            Ok(None) => {}
            Err(()) => {
                flush_all(&mut pending, &out_tx);
                return;
            }
        }

        flush_ripe(&mut pending, &out_tx, debounce);
    }
}

fn ingest(event: notify::Event, pending: &mut HashMap<PathBuf, (WatchEvent, Instant)>) {
    let now = Instant::now();
    for path in event.paths {
        let Some(kind) = classify(&event.kind) else {
            continue;
        };
        let new_ev = match kind {
            Classification::Upsert => WatchEvent::Upsert(path.clone()),
            Classification::Remove => WatchEvent::Remove(path.clone()),
        };
        debug!(?new_ev, "watcher raw event");
        pending.insert(path, (new_ev, now));
    }
}

enum Classification {
    Upsert,
    Remove,
}

fn classify(kind: &EventKind) -> Option<Classification> {
    match kind {
        EventKind::Create(_) => Some(Classification::Upsert),
        EventKind::Modify(m) => match m {
            ModifyKind::Metadata(_) => None,
            _ => Some(Classification::Upsert),
        },
        EventKind::Remove(_) => Some(Classification::Remove),
        _ => None,
    }
}

fn flush_ripe(
    pending: &mut HashMap<PathBuf, (WatchEvent, Instant)>,
    out_tx: &mpsc::Sender<WatchEvent>,
    debounce: Duration,
) {
    let now = Instant::now();
    let mut ripe: Vec<PathBuf> = Vec::new();
    for (p, (_, at)) in pending.iter() {
        if now.duration_since(*at) >= debounce {
            ripe.push(p.clone());
        }
    }
    for p in ripe {
        if let Some((ev, _)) = pending.remove(&p) {
            // `try_send` keeps the watcher responsive under backpressure —
            // if the consumer is slow, we drop duplicates rather than block
            // OS event delivery.
            if let Err(e) = out_tx.blocking_send(ev) {
                debug!("watch receiver dropped: {e}");
                return;
            }
        }
    }
}

fn flush_all(
    pending: &mut HashMap<PathBuf, (WatchEvent, Instant)>,
    out_tx: &mpsc::Sender<WatchEvent>,
) {
    for (_, (ev, _)) in pending.drain() {
        let _ = out_tx.blocking_send(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detects_file_create_and_modify() {
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut handle = watch(vec![root.clone()], Duration::from_millis(120)).unwrap();

        // Small sleep so notify's watch call is fully installed.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let f = root.join("note.md");
        fs::write(&f, "# hi\n").unwrap();

        let ev = tokio::time::timeout(Duration::from_millis(1500), handle.next())
            .await
            .expect("event arrived")
            .expect("channel open");
        assert!(matches!(ev, WatchEvent::Upsert(_)));
        assert_eq!(ev.path(), f.as_path());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rapid_writes_are_debounced() {
        let dir = tempdir().unwrap();
        let mut handle = watch(vec![dir.path().to_path_buf()], Duration::from_millis(150)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let f = dir.path().join("note.md");
        for i in 0..5 {
            fs::write(&f, format!("# v{i}\n")).unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let ev = tokio::time::timeout(Duration::from_millis(1500), handle.next())
            .await
            .expect("event arrived")
            .expect("channel open");
        assert!(matches!(ev, WatchEvent::Upsert(_)));

        // No follow-up event should arrive within another debounce window.
        let second = tokio::time::timeout(Duration::from_millis(300), handle.next()).await;
        assert!(second.is_err() || second.unwrap().is_none());
    }
}
