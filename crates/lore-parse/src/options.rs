//! Single source of truth for the `pulldown-cmark` option set.
//!
//! Every pass we make over a document — headings, inline links, code-fence
//! mask, Dataview detection, summary flattening — must use identical
//! options or offsets from different passes can disagree. Centralizing
//! here guarantees they stay in sync.

use pulldown_cmark::Options;

/// Returns the options Lore uses for every markdown parse.
///
/// Why a function rather than a `const`: `pulldown_cmark::Options` is
/// internally a bitflags type; `Options::empty().insert(...)` chains
/// can't run in a `const` context on stable. Inlining is cheap.
#[inline]
pub fn parser_options() -> Options {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts
}
