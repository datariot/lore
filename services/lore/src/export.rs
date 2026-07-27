//! `llms.txt` export.
//!
//! [llms.txt](https://llmstxt.org/) is an adopted convention: a Markdown file
//! at a known path giving an LLM a curated, link-first map of a site's or
//! corpus's content. Lore already holds exactly what the format wants — a
//! title, per-document hooks, and folder structure — so exporting one is a
//! pure projection over the index. This makes Lore useful even to agents that
//! never speak MCP: point them at `llms.txt` and they get the same structure
//! the retrieval tools expose.
//!
//! Two artifacts:
//! - `llms.txt` — H1 title, a blockquote blurb, then one `## folder` section
//!   per top-level directory, each a bullet list of `[title](rel_path): note`.
//! - `llms-full.txt` (with `--full`) — every document's full text concatenated
//!   in the same order, for agents that want the whole corpus inline.

use std::collections::BTreeMap;
use std::path::Path;

use lore_core::{Error, Result};
use lore_index::CorpusIndex;

use crate::cli::{IndexOptions, index_command};
use crate::config::index_path;
use crate::mcp::CorpusRegistry;

/// Root-level pseudo-folder key. Sorts before any real folder so root
/// documents lead the file.
const ROOT_SECTION: &str = "";

/// Render `llms.txt` for a corpus: pure projection over the index.
pub fn render_llms_txt(corpus: &CorpusIndex) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", corpus.source));
    out.push_str(&format!("> {}\n\n", corpus_blurb(corpus)));
    out.push_str(&format!(
        "Index of {} documents. Each link is a path relative to the corpus root.\n",
        corpus.documents.len()
    ));

    // Group documents by top-level folder for stable, sectioned output.
    let mut sections: BTreeMap<&str, Vec<&lore_index::DocumentIndex>> = BTreeMap::new();
    for doc in &corpus.documents {
        let folder = doc
            .rel_path
            .split_once('/')
            .map(|(f, _)| f)
            .unwrap_or(ROOT_SECTION);
        sections.entry(folder).or_default().push(doc);
    }

    for (folder, mut docs) in sections {
        docs.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        let heading = if folder == ROOT_SECTION {
            "Documents".to_string()
        } else {
            format!("{folder}/")
        };
        out.push_str(&format!("\n## {heading}\n\n"));
        for doc in docs {
            let title = doc_title(doc);
            match doc_note(doc) {
                Some(note) => out.push_str(&format!("- [{title}]({}): {note}\n", doc.rel_path)),
                None => out.push_str(&format!("- [{title}]({})\n", doc.rel_path)),
            }
        }
    }
    out
}

/// Render `llms-full.txt`: every document's full text, in the same folder
/// order as `llms.txt`, each under an `# <rel_path>` heading. Reads files from
/// `root` (the index stores byte ranges, not content).
pub fn render_llms_full(corpus: &CorpusIndex, root: &Path) -> Result<String> {
    let mut docs: Vec<&lore_index::DocumentIndex> = corpus.documents.iter().collect();
    docs.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    let mut out = String::new();
    out.push_str(&format!("# {} — full text\n\n", corpus.source));
    for doc in docs {
        let path = root.join(&doc.rel_path);
        let content = std::fs::read_to_string(&path)
            .map_err(|e| Error::Io(format!("read {}: {e}", path.display())))?;
        out.push_str(&format!("\n\n---\n\n# {}\n\n", doc.rel_path));
        out.push_str(content.trim_end());
        out.push('\n');
    }
    Ok(out)
}

/// The blockquote blurb: a root README/index document's description or first
/// summary, else a generic count line.
fn corpus_blurb(corpus: &CorpusIndex) -> String {
    let readme = corpus.documents.iter().find(|d| {
        let lower = d.rel_path.to_ascii_lowercase();
        matches!(lower.as_str(), "readme.md" | "index.md" | "readme.markdown")
    });
    if let Some(doc) = readme {
        if let Some(desc) = doc.description() {
            return desc.to_string();
        }
        if let Some(first) = doc.nodes.first()
            && !first.summary.is_empty()
        {
            return first.summary.clone();
        }
    }
    format!(
        "A Lore-indexed corpus of {} documents.",
        corpus.documents.len()
    )
}

/// Document title: its author `description` is the best hook, else the first
/// heading, else the file stem.
fn doc_title(doc: &lore_index::DocumentIndex) -> String {
    if let Some(node) = doc.nodes.first()
        && !node.title.is_empty()
    {
        return node.title.clone();
    }
    doc.rel_path
        .rsplit('/')
        .next()
        .unwrap_or(&doc.rel_path)
        .to_string()
}

/// The note after a link: frontmatter description, else the first section's
/// summary. Whitespace-collapsed and length-capped so the list stays scannable.
fn doc_note(doc: &lore_index::DocumentIndex) -> Option<String> {
    let raw = doc
        .description()
        .map(str::to_string)
        .or_else(|| doc.nodes.first().map(|n| n.summary.clone()))
        .filter(|s| !s.is_empty())?;
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(truncate_chars(&collapsed, 160))
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// CLI entry point: ensure the corpus is indexed, load it, and write `llms.txt`
/// (and `llms-full.txt` when `full`) into `out_dir`, or print `llms.txt` to
/// stdout when `out_dir` is None.
pub fn export_command(root: &Path, out_dir: Option<&Path>, full: bool) -> Result<()> {
    let idx = index_path(root);
    if !idx.exists() {
        index_command(IndexOptions::new(root.to_path_buf()))?;
    }
    let registry = CorpusRegistry::new();
    let handle = registry.load_from_root(root)?;
    let corpus = handle.read();

    let llms = render_llms_txt(&corpus);
    match out_dir {
        None => {
            print!("{llms}");
            if full {
                return Err(Error::Io(
                    "--full requires --out (llms-full.txt is written to a directory)".to_string(),
                ));
            }
        }
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Io(format!("create {}: {e}", dir.display())))?;
            let llms_path = dir.join("llms.txt");
            std::fs::write(&llms_path, &llms)
                .map_err(|e| Error::Io(format!("write {}: {e}", llms_path.display())))?;
            println!("wrote {}", llms_path.display());
            if full {
                let full_txt = render_llms_full(&corpus, root)?;
                let full_path = dir.join("llms-full.txt");
                std::fs::write(&full_path, &full_txt)
                    .map_err(|e| Error::Io(format!("write {}: {e}", full_path.display())))?;
                println!("wrote {}", full_path.display());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lore_core::SourceId;
    use lore_index::build_document;
    use std::path::PathBuf;

    fn corpus() -> CorpusIndex {
        let mut c = CorpusIndex::new(SourceId::new("demo"), PathBuf::from("/tmp"));
        c.push_document(
            build_document(
                SourceId::new("demo"),
                "README.md",
                "---\ndescription: The demo corpus.\n---\n# Demo\n\nWelcome.\n",
            )
            .unwrap(),
        );
        c.push_document(
            build_document(
                SourceId::new("demo"),
                "docs/intro.md",
                "# Introduction\n\nHook sentence here. More prose.\n",
            )
            .unwrap(),
        );
        c.push_document(
            build_document(
                SourceId::new("demo"),
                "docs/guide.md",
                "# Guide\n\nSteps.\n",
            )
            .unwrap(),
        );
        c.rebuild_indices();
        c
    }

    #[test]
    fn llms_txt_has_spec_shape() {
        let out = render_llms_txt(&corpus());
        // H1 title, then a blockquote blurb sourced from README's description.
        assert!(out.starts_with("# demo\n"));
        assert!(out.contains("> The demo corpus."));
        // A section per top-level folder, plus root docs under "Documents".
        assert!(out.contains("## Documents\n"));
        assert!(out.contains("## docs/\n"));
        // Links are rel_paths; notes come from description/summary.
        assert!(out.contains("- [Demo](README.md): The demo corpus."));
        assert!(out.contains("[Introduction](docs/intro.md): Hook sentence here."));
    }

    #[test]
    fn root_section_sorts_before_folders() {
        let out = render_llms_txt(&corpus());
        let docs_at = out.find("## Documents").unwrap();
        let folder_at = out.find("## docs/").unwrap();
        assert!(docs_at < folder_at, "root documents lead the file");
    }

    #[test]
    fn export_command_writes_both_files() {
        use std::fs;
        let src = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("docs")).unwrap();
        fs::write(src.path().join("README.md"), "# Root\n\nHello.\n").unwrap();
        fs::write(
            src.path().join("docs/a.md"),
            "# Alpha\n\nZephyrine content here.\n",
        )
        .unwrap();

        let out = tempfile::tempdir().unwrap();
        export_command(src.path(), Some(out.path()), true).unwrap();

        let llms = fs::read_to_string(out.path().join("llms.txt")).unwrap();
        assert!(llms.contains("[Alpha](docs/a.md)"));

        let full = fs::read_to_string(out.path().join("llms-full.txt")).unwrap();
        assert!(full.contains("# docs/a.md"), "full text sections by path");
        assert!(
            full.contains("Zephyrine content here."),
            "full text inlines bodies"
        );
    }

    #[test]
    fn note_collapses_whitespace_and_caps_length() {
        let long = "word ".repeat(80);
        let src = format!("# T\n\n{long}\n");
        let mut c = CorpusIndex::new(SourceId::new("x"), PathBuf::from("/tmp"));
        c.push_document(build_document(SourceId::new("x"), "a.md", &src).unwrap());
        c.rebuild_indices();
        let note = doc_note(&c.documents[0]).unwrap();
        assert!(note.chars().count() <= 160);
        assert!(!note.contains("  "), "whitespace collapsed");
    }
}
