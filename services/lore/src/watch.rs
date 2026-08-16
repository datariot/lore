//! Bridge `lore-watch` events into `CorpusRegistry` updates.
//!
//! `lore watch` runs the MCP server and, in parallel, a debounced
//! filesystem watcher over every registered corpus root. Each event is
//! mapped back to `(source_id, rel_path)` and routed to either
//! `reindex_document` or `remove_document`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use lore_core::{Result, SourceId};
use lore_watch::{WatchEvent, watch};
use tracing::{debug, info, warn};

use crate::config::MARKDOWN_EXTENSIONS;
use crate::mcp::CorpusRegistry;
use crate::walker::{PathFilter, WalkOptions};

/// Default debounce window. Editors emit bursty events (Vim's atomic-save
/// writes a temp, renames it, then touches mtime). 250 ms comfortably
/// collapses those into one re-index.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(250);

pub async fn run_watcher(registry: CorpusRegistry, debounce: Duration) -> Result<()> {
    let roots = registry.roots();
    if roots.is_empty() {
        warn!("no corpus roots to watch — aborting watcher");
        return Ok(());
    }
    let paths: Vec<PathBuf> = roots.iter().map(|(_sid, root)| root.clone()).collect();

    // One filter per corpus, holding the same walk rules the initial index
    // used. Without this the watcher would happily splice in files the
    // indexer skipped — see `PathFilter`.
    let filters: HashMap<SourceId, PathFilter> = roots
        .into_iter()
        .map(|(sid, root)| (sid, PathFilter::new(root, WalkOptions::default())))
        .collect();

    let mut handle = watch(paths, debounce)?;
    info!("filesystem watcher started");

    while let Some(event) = handle.next().await {
        let path = event.path().to_path_buf();

        // Ignore files are not markdown, but they change what counts as
        // markdown-we-care-about, so they are handled before the extension
        // check. We drop the cached matchers rather than re-walking: the
        // corpus itself is only corrected on the next `add_source(rebuild)`.
        if PathFilter::is_ignore_file(&path) {
            for filter in filters.values() {
                if path.starts_with(filter.root()) {
                    filter.invalidate();
                    info!(
                        path = %path.display(),
                        "ignore rules changed — matchers invalidated; \
                         rebuild the source to re-apply them to existing documents"
                    );
                }
            }
            continue;
        }

        if !is_markdown(&path) {
            debug!(path = %path.display(), "ignoring non-markdown change");
            continue;
        }
        let Some((source, rel)) = registry.locate(&path) else {
            debug!(path = %path.display(), "change outside any known corpus root");
            continue;
        };

        // `rel` came from `locate`, which canonicalizes; rejoin onto the
        // filter's own root so the two agree on prefix without re-statting.
        if let Some(filter) = filters.get(&source)
            && !filter.accepts(&filter.root().join(&rel))
        {
            debug!(%source, rel, "change is excluded by walk rules — not indexing");
            continue;
        }

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
