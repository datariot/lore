//! Bridge `lore-watch` events into `CorpusRegistry` updates.
//!
//! `lore watch` runs the MCP server and, in parallel, a debounced
//! filesystem watcher over every registered corpus root. Each event is
//! mapped back to `(source_id, rel_path)` and routed to either
//! `reindex_document` or `remove_document`.

use std::path::PathBuf;
use std::time::Duration;

use lore_core::Result;
use lore_watch::{WatchEvent, watch};
use tracing::{debug, info, warn};

use crate::config::MARKDOWN_EXTENSIONS;
use crate::mcp::CorpusRegistry;

/// Default debounce window. Editors emit bursty events (Vim's atomic-save
/// writes a temp, renames it, then touches mtime). 250 ms comfortably
/// collapses those into one re-index.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(250);

pub async fn run_watcher(registry: CorpusRegistry, debounce: Duration) -> Result<()> {
    let paths: Vec<PathBuf> = registry
        .roots()
        .into_iter()
        .map(|(_sid, root)| root)
        .collect();
    if paths.is_empty() {
        warn!("no corpus roots to watch — aborting watcher");
        return Ok(());
    }

    let mut handle = watch(paths, debounce)?;
    info!("filesystem watcher started");

    while let Some(event) = handle.next().await {
        let path = event.path().to_path_buf();
        if !is_markdown(&path) {
            debug!(path = %path.display(), "ignoring non-markdown change");
            continue;
        }
        let Some((source, rel)) = registry.locate(&path) else {
            debug!(path = %path.display(), "change outside any known corpus root");
            continue;
        };

        // Some backends (notably macOS FSEvents) deliver deletes as a
        // Modify-then-nothing rather than a Remove event. Normalize by
        // checking on-disk state.
        let exists = path.exists();
        let resolved = match event {
            WatchEvent::Upsert(_) if !exists => WatchEvent::Remove(path.clone()),
            other => other,
        };

        match resolved {
            WatchEvent::Upsert(_) => match registry.reindex_document(&source, &rel) {
                Ok(()) => info!(%source, rel, "reindexed on change"),
                Err(e) => warn!(%source, rel, err = %e, "reindex failed"),
            },
            WatchEvent::Remove(_) => {
                registry.remove_document(&source, &rel);
                info!(%source, rel, "removed on delete");
            }
        }
    }
    Ok(())
}

fn is_markdown(path: &std::path::Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let lower = ext.to_lowercase();
    MARKDOWN_EXTENSIONS.iter().any(|m| *m == lower)
}
