//! Debug helper: print the heading tree of a markdown file.
//!
//! ```bash
//! cargo run -p lore-index --example dump_tree -- path/to/file.md
//! ```

use std::fs;
use std::process::ExitCode;

use lore_core::SourceId;
use lore_index::build_document;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: dump_tree <file.md>");
        return ExitCode::from(2);
    };
    let src = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read {path}: {e}");
            return ExitCode::from(1);
        }
    };
    let doc = match build_document(SourceId::new("example"), &path, &src) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("parse {path}: {e}");
            return ExitCode::from(1);
        }
    };
    for node in &doc.nodes {
        let indent = "  ".repeat(node.level.saturating_sub(1) as usize);
        println!(
            "{indent}{id} [{range}] {title}",
            id = node.id,
            range = format_range(node.byte_range),
            title = node.title,
        );
        if !node.summary.is_empty() {
            println!("{indent}    └── {}", node.summary);
        }
    }
    ExitCode::SUCCESS
}

fn format_range(r: lore_core::ByteRange) -> String {
    format!("{}..{}", r.start, r.end)
}
