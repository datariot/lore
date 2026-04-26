//! `LoreServer` — the `rmcp`-facing tool router.

use std::path::PathBuf;

use std::collections::HashSet;

use lore_core::{Error, LinkKind, NodeId, SourceId};
use lore_index::{DocId, DocumentIndex, HeadingNode, canonical_link_keys};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData as McpError, Json, ServerHandler, tool, tool_handler, tool_router};
use tracing::info;

use crate::cli::{IndexOptions, index_command};
use crate::config::index_path;
use crate::mcp::registry::CorpusRegistry;
use crate::mcp::tools::{
    AddSourceRequest, AddSourceResponse, Backlink, BacklinksRequest, BacklinksResponse,
    DocumentSummary, GetByPathRequest, GetSectionRequest, GroupBy, HotNode, HotRequest,
    HotResponse, LinkInfo, ListDocumentsRequest, ListDocumentsResponse, ListSourcesResponse,
    NeighborRef, NeighborsRequest, NeighborsResponse, ResolvedNode, SearchHit, SearchRequest,
    SearchResponse, SectionHit, SectionResponse, SourceSummary, TocDocument, TocRequest,
    TocResponse, flatten_toc, frontmatter_matches, to_heading_path,
};

#[derive(Clone)]
pub struct LoreServer {
    registry: CorpusRegistry,
    tool_router: ToolRouter<LoreServer>,
}

#[tool_router]
impl LoreServer {
    pub fn new(registry: CorpusRegistry) -> Self {
        Self {
            registry,
            tool_router: Self::tool_router(),
        }
    }

    /// Look up a loaded corpus by its string id, returning an MCP-facing
    /// error when missing. Every tool that reads a corpus starts this way.
    fn corpus_handle(
        &self,
        source_id: &str,
    ) -> Result<crate::mcp::registry::CorpusHandle, McpError> {
        self.registry
            .get(&SourceId::new(source_id))
            .ok_or_else(|| source_not_found(source_id))
    }

    // ---- list_sources -------------------------------------------------------

    #[tool(
        description = "List every corpus Lore has loaded. Returns the source identifier, root directory, and document/heading counts for each."
    )]
    async fn list_sources(&self) -> Result<Json<ListSourcesResponse>, McpError> {
        let mut sources = Vec::with_capacity(self.registry.len());
        for id in self.registry.ids() {
            if let Some(handle) = self.registry.get(&id) {
                let c = handle.read();
                sources.push(SourceSummary {
                    source_id: c.source.to_string(),
                    root_dir: c.root_dir.display().to_string(),
                    documents: c.documents.len(),
                    nodes: c.total_nodes(),
                });
            }
        }
        Ok(Json(ListSourcesResponse { sources }))
    }

    // ---- list_documents -----------------------------------------------------

    #[tool(
        description = "List documents in a corpus, optionally filtered by `path_prefix` and by frontmatter equality (e.g. `{\"type\":\"moc\"}` or `{\"tags\":\"project\"}` to match an array element). Cheaper than `table_of_contents` when the agent only needs to discover which files exist or filter by metadata."
    )]
    async fn list_documents(
        &self,
        Parameters(req): Parameters<ListDocumentsRequest>,
    ) -> Result<Json<ListDocumentsResponse>, McpError> {
        let handle = self.corpus_handle(&req.source_id)?;
        let corpus = handle.read();

        let prefix = req.path_prefix.as_deref();
        let filters = req.frontmatter.as_ref();

        let mut matched = 0usize;
        let mut documents: Vec<DocumentSummary> = Vec::new();
        for (i, doc) in corpus.documents.iter().enumerate() {
            if let Some(p) = prefix
                && !doc.rel_path.starts_with(p)
            {
                continue;
            }
            if let Some(f) = filters
                && !frontmatter_matches(f, doc.frontmatter.as_ref())
            {
                continue;
            }
            matched += 1;
            if documents.len() >= req.limit {
                continue;
            }
            documents.push(DocumentSummary {
                rel_path: doc.rel_path.clone(),
                doc_id: i as u32,
                title: doc.nodes.first().map(|n| n.title.clone()),
                node_count: doc.nodes.len(),
                frontmatter: if req.include_frontmatter {
                    doc.frontmatter.clone()
                } else {
                    None
                },
            });
        }

        Ok(Json(ListDocumentsResponse {
            source_id: corpus.source.to_string(),
            truncated: matched > documents.len(),
            documents,
        }))
    }

    // ---- table_of_contents --------------------------------------------------

    #[tool(
        description = "Return the heading tree for a corpus — optionally narrowed to one document and capped at a given depth. The preferred first call to understand what's in a source."
    )]
    async fn table_of_contents(
        &self,
        Parameters(req): Parameters<TocRequest>,
    ) -> Result<Json<TocResponse>, McpError> {
        let handle = self.corpus_handle(&req.source_id)?;
        let corpus = handle.read();

        let documents: Vec<TocDocument> = match req.rel_path.as_deref() {
            Some(rel) => {
                let doc = corpus
                    .documents
                    .iter()
                    .enumerate()
                    .find(|(_, d)| d.rel_path == rel)
                    .ok_or_else(|| doc_not_found(&req.source_id, rel))?;
                vec![doc_to_toc(
                    doc.0 as u32,
                    doc.1,
                    req.max_depth,
                    req.include_frontmatter,
                )]
            }
            None => {
                let prefix = req.path_prefix.as_deref();
                corpus
                    .documents
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| match prefix {
                        Some(p) => d.rel_path.starts_with(p),
                        None => true,
                    })
                    .map(|(i, d)| doc_to_toc(i as u32, d, req.max_depth, req.include_frontmatter))
                    .collect()
            }
        };

        Ok(Json(TocResponse {
            source_id: corpus.source.to_string(),
            documents,
        }))
    }

    // ---- get_section --------------------------------------------------------

    #[tool(
        description = "Retrieve the content of a section by heading path or node id. Uses a cached memory map of the source file so reads are O(1) byte-range slices — no markdown reparsing."
    )]
    async fn get_section(
        &self,
        Parameters(req): Parameters<GetSectionRequest>,
    ) -> Result<Json<SectionResponse>, McpError> {
        let source = SourceId::new(&req.source_id);
        let handle = self.corpus_handle(&req.source_id)?;
        let corpus = handle.read();

        let resolved = resolve_node(&corpus, &req)?;
        let range = if req.body_only {
            resolved.node.content_range
        } else {
            resolved.node.byte_range
        };

        let map = self
            .registry
            .mmap_document(&source, &req.rel_path)
            .map_err(to_mcp_err)?;
        let slice = &map[range.start as usize..(range.end as usize).min(map.len())];
        let content = std::str::from_utf8(slice).map_err(|e| mcp_internal(format!("utf8: {e}")))?;

        resolved.node.access_count.bump();

        Ok(Json(SectionResponse {
            source_id: corpus.source.to_string(),
            rel_path: req.rel_path,
            node_id: resolved.node_id.0,
            level: resolved.node.level,
            path: resolved.node.path.0.clone(),
            byte_range: [range.start, range.end],
            content: content.to_string(),
            outbound_links: resolved
                .node
                .outbound_links
                .iter()
                .map(|l| LinkInfo {
                    target: l.target.clone(),
                    kind: match l.kind {
                        LinkKind::Inline => "inline".to_string(),
                        LinkKind::Wiki => "wiki".to_string(),
                    },
                    text: l.text.clone(),
                })
                .collect(),
        }))
    }

    // ---- search -------------------------------------------------------------

    #[tool(
        description = "BM25 keyword search over heading titles, path segments, and the per-section first-sentence summary. Returns ranked hits with a summary line each. Tokens are lowercased; English stopwords and tokens shorter than two characters are skipped. Prefix a token with `-` to exclude any node containing it (e.g., `kafka -lambda`). No phrase or proximity operators. Set `group_by` to `\"doc\"` to collapse same-document hits into one primary plus up to `secondary_limit` (default 3) nested same-document sections — useful for narrow queries that concentrate in a single file."
    )]
    async fn search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<Json<SearchResponse>, McpError> {
        let handle = self.corpus_handle(&req.source_id)?;
        let corpus = handle.read();

        let hits = match req.group_by {
            GroupBy::Section => lore_search::search(&corpus, &req.query, req.limit)
                .into_iter()
                .filter_map(|h| {
                    let doc = corpus.doc(h.doc)?;
                    let node = doc.node(h.node)?;
                    Some(SearchHit {
                        rel_path: doc.rel_path.clone(),
                        doc_id: h.doc.0,
                        node_id: h.node.0,
                        level: node.level,
                        path: node.path.0.clone(),
                        summary: node.summary.clone(),
                        score: h.score,
                        secondary_hits: Vec::new(),
                    })
                })
                .collect(),
            GroupBy::Doc => {
                lore_search::search_grouped(&corpus, &req.query, req.limit, req.secondary_limit)
                    .into_iter()
                    .filter_map(|g| {
                        let doc = corpus.doc(g.primary.doc)?;
                        let primary_node = doc.node(g.primary.node)?;
                        let secondary_hits = g
                            .secondary
                            .into_iter()
                            .filter_map(|s| {
                                let n = doc.node(s.node)?;
                                Some(SectionHit {
                                    node_id: s.node.0,
                                    level: n.level,
                                    path: n.path.0.clone(),
                                    summary: n.summary.clone(),
                                    score: s.score,
                                })
                            })
                            .collect();
                        Some(SearchHit {
                            rel_path: doc.rel_path.clone(),
                            doc_id: g.primary.doc.0,
                            node_id: g.primary.node.0,
                            level: primary_node.level,
                            path: primary_node.path.0.clone(),
                            summary: primary_node.summary.clone(),
                            score: g.primary.score,
                            secondary_hits,
                        })
                    })
                    .collect()
            }
        };

        Ok(Json(SearchResponse {
            source_id: corpus.source.to_string(),
            query: req.query,
            hits,
        }))
    }

    // ---- backlinks ----------------------------------------------------------

    #[tool(
        description = "Return every section that links *to* `target` inside the corpus. Precomputed at index time — O(1) lookup."
    )]
    async fn backlinks(
        &self,
        Parameters(req): Parameters<BacklinksRequest>,
    ) -> Result<Json<BacklinksResponse>, McpError> {
        let handle = self.corpus_handle(&req.source_id)?;
        let corpus = handle.read();
        // Index time stores each link under multiple canonical keys
        // (basename, with/without extension, with/without #fragment). Run the
        // same canonicalization on the query so all link spellings of the
        // same logical target return the same set.
        let mut seen: HashSet<(DocId, NodeId)> = HashSet::new();
        let mut out: Vec<Backlink> = Vec::new();
        for key in canonical_link_keys(&req.target) {
            let Some(postings) = corpus.backlinks.get(&key) else {
                continue;
            };
            for &(did, nid) in postings {
                if !seen.insert((did, nid)) {
                    continue;
                }
                let Some(doc) = corpus.doc(did) else { continue };
                let Some(node) = doc.node(nid) else { continue };
                out.push(Backlink {
                    rel_path: doc.rel_path.clone(),
                    doc_id: did.0,
                    node_id: nid.0,
                    level: node.level,
                    path: node.path.0.clone(),
                    summary: node.summary.clone(),
                });
                if out.len() >= req.limit {
                    break;
                }
            }
            if out.len() >= req.limit {
                break;
            }
        }
        Ok(Json(BacklinksResponse {
            source_id: corpus.source.to_string(),
            target: req.target,
            backlinks: out,
        }))
    }

    // ---- recent_hot ---------------------------------------------------------

    #[tool(
        description = "Top-N sections in a corpus by access count. Usage reveals which parts of the documentation an agent actually relies on — a ByteRover-style signal with zero curation."
    )]
    async fn recent_hot(
        &self,
        Parameters(req): Parameters<HotRequest>,
    ) -> Result<Json<HotResponse>, McpError> {
        let handle = self.corpus_handle(&req.source_id)?;
        let corpus = handle.read();
        let mut all: Vec<(DocId, &lore_index::HeadingNode, u32)> =
            Vec::with_capacity(corpus.total_nodes());
        for (di, doc) in corpus.documents.iter().enumerate() {
            let did = DocId(di as u32);
            for node in &doc.nodes {
                let count = node.access_count.get();
                if count > 0 {
                    all.push((did, node, count));
                }
            }
        }
        all.sort_by(|a, b| b.2.cmp(&a.2));
        all.truncate(req.limit);
        let nodes = all
            .into_iter()
            .filter_map(|(did, node, count)| {
                let doc = corpus.doc(did)?;
                Some(HotNode {
                    rel_path: doc.rel_path.clone(),
                    doc_id: did.0,
                    node_id: node.id.0,
                    level: node.level,
                    path: node.path.0.clone(),
                    summary: node.summary.clone(),
                    access_count: count,
                })
            })
            .collect();
        Ok(Json(HotResponse {
            source_id: corpus.source.to_string(),
            nodes,
        }))
    }

    // ---- neighbors ----------------------------------------------------------

    #[tool(
        description = "Return a node's parent, previous sibling, next sibling, and children. Lets an agent navigate one hop at a time without refetching the TOC."
    )]
    async fn neighbors(
        &self,
        Parameters(req): Parameters<NeighborsRequest>,
    ) -> Result<Json<NeighborsResponse>, McpError> {
        let handle = self.corpus_handle(&req.source_id)?;
        let corpus = handle.read();

        let (doc_idx, doc) = corpus
            .documents
            .iter()
            .enumerate()
            .find(|(_, d)| d.rel_path == req.rel_path)
            .ok_or_else(|| doc_not_found(&req.source_id, &req.rel_path))?;
        let _ = doc_idx;

        let node = resolve_in_doc(doc, req.heading_path.as_deref(), req.node_id)?;
        let parent = node.parent.and_then(|pid| doc.node(pid)).map(to_ref);

        let siblings: &[NodeId] = match node.parent {
            Some(pid) => doc
                .node(pid)
                .map(|p| p.children.as_slice())
                .unwrap_or_default(),
            None => doc.roots.as_slice(),
        };
        let pos = siblings.iter().position(|&n| n == node.id);
        let prev_sibling = pos
            .and_then(|p| p.checked_sub(1))
            .and_then(|i| siblings.get(i))
            .and_then(|&nid| doc.node(nid))
            .map(to_ref);
        let next_sibling = pos
            .and_then(|p| siblings.get(p + 1))
            .and_then(|&nid| doc.node(nid))
            .map(to_ref);

        let children: Vec<NeighborRef> = node
            .children
            .iter()
            .filter_map(|&cid| doc.node(cid).map(to_ref))
            .collect();

        Ok(Json(NeighborsResponse {
            source_id: corpus.source.to_string(),
            rel_path: req.rel_path,
            node_id: node.id.0,
            parent,
            prev_sibling,
            next_sibling,
            children,
        }))
    }

    // ---- get_by_path --------------------------------------------------------

    #[tool(
        description = "Fetch a section by a single qualified path string of the form `path/to/file.md#Heading > Subheading`. Convenient wrapper around `get_section` for clients that already carry paths around."
    )]
    async fn get_by_path(
        &self,
        Parameters(req): Parameters<GetByPathRequest>,
    ) -> Result<Json<SectionResponse>, McpError> {
        let (rel, heading) = parse_qualified_path(&req.qualified_path);
        let base = GetSectionRequest {
            source_id: req.source_id.clone(),
            rel_path: rel.to_string(),
            heading_path: heading,
            node_id: None,
            body_only: req.body_only,
        };
        self.get_section(Parameters(base)).await
    }

    // ---- add_source ---------------------------------------------------------

    #[tool(
        description = "Index a directory of markdown files and register the result as a new corpus. If `.lore/index.json` already exists and `rebuild` is false, loads from the existing index."
    )]
    async fn add_source(
        &self,
        Parameters(req): Parameters<AddSourceRequest>,
    ) -> Result<Json<AddSourceResponse>, McpError> {
        let root = PathBuf::from(&req.root);
        let idx_path = index_path(&root);
        let indexed = if req.rebuild || !idx_path.exists() {
            let mut opts = IndexOptions::new(root.clone());
            opts.source_id = req.source_id.clone();
            let report = tokio::task::spawn_blocking(move || index_command(opts))
                .await
                .map_err(|e| mcp_internal(format!("join: {e}")))?
                .map_err(to_mcp_err)?;
            info!(
                source = %report.source_id,
                docs = report.files_indexed,
                nodes = report.total_nodes,
                "indexed via MCP"
            );
            true
        } else {
            false
        };

        let handle = self
            .registry
            .load_from_path(&idx_path)
            .map_err(to_mcp_err)?;
        let corpus = handle.read();

        Ok(Json(AddSourceResponse {
            source_id: corpus.source.to_string(),
            root_dir: corpus.root_dir.display().to_string(),
            documents: corpus.documents.len(),
            nodes: corpus.total_nodes(),
            indexed,
        }))
    }
}

#[tool_handler]
impl ServerHandler for LoreServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "lore".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                title: Some("Lore".to_string()),
                website_url: Some("https://github.com/datariot/lore".to_string()),
                icons: None,
            },
            instructions: Some(
                "Lore exposes structured retrieval over indexed markdown corpora. \
                 Start with `list_sources` to see what's loaded, call `table_of_contents` \
                 to get a document's heading tree, then use `get_section` to pull exact \
                 byte-range content without reparsing. `search` covers keyword lookups. \
                 `add_source` registers a new directory."
                    .to_string(),
            ),
        }
    }
}

// -----------------------------------------------------------------------------
// helpers
// -----------------------------------------------------------------------------

fn doc_to_toc(
    doc_id: u32,
    doc: &DocumentIndex,
    max_depth: Option<u8>,
    include_frontmatter: bool,
) -> TocDocument {
    TocDocument {
        rel_path: doc.rel_path.clone(),
        doc_id,
        entries: flatten_toc(doc, max_depth),
        frontmatter: if include_frontmatter {
            doc.frontmatter.clone()
        } else {
            None
        },
    }
}

/// Find the `(DocId, &DocumentIndex)` for a given `rel_path`, or an error.
fn find_doc<'a>(
    corpus: &'a lore_index::CorpusIndex,
    source_id: &str,
    rel_path: &str,
) -> Result<(DocId, &'a DocumentIndex), McpError> {
    corpus
        .documents
        .iter()
        .enumerate()
        .find(|(_, d)| d.rel_path == rel_path)
        .map(|(i, d)| (DocId(i as u32), d))
        .ok_or_else(|| doc_not_found(source_id, rel_path))
}

/// Resolve a node within a single document by either `node_id` or
/// `heading_path`. Shared by `get_section`, `neighbors`, and `get_by_path`.
fn resolve_in_doc<'a>(
    doc: &'a DocumentIndex,
    heading_path: Option<&[String]>,
    node_id: Option<u32>,
) -> Result<&'a HeadingNode, McpError> {
    if let Some(nid) = node_id {
        return doc
            .node(NodeId(nid))
            .ok_or_else(|| mcp_invalid(format!("node_id {nid} out of range")));
    }
    if let Some(segs) = heading_path {
        let target = to_heading_path(segs);
        return doc
            .nodes
            .iter()
            .find(|n| n.path == target)
            .ok_or_else(|| mcp_not_found(format!("no heading `{target}`")));
    }
    Err(mcp_invalid(
        "one of `node_id` or `heading_path` is required".to_string(),
    ))
}

/// Full resolution for `get_section`-style requests: both the enclosing
/// document and the node within it.
fn resolve_node<'a>(
    corpus: &'a lore_index::CorpusIndex,
    req: &GetSectionRequest,
) -> Result<ResolvedNode<'a>, McpError> {
    let (doc_id, doc) = find_doc(corpus, &req.source_id, &req.rel_path)?;
    let node = resolve_in_doc(doc, req.heading_path.as_deref(), req.node_id)?;
    Ok(ResolvedNode {
        doc,
        node,
        doc_id,
        node_id: node.id,
    })
}

fn source_not_found(id: &str) -> McpError {
    McpError::invalid_params(format!("source `{id}` not loaded"), None)
}

fn doc_not_found(source: &str, rel: &str) -> McpError {
    McpError::invalid_params(format!("no document `{rel}` in source `{source}`"), None)
}

/// Map `lore_core::Error` to the rmcp wire error. We can't `impl From`
/// because both types are foreign to this crate; a named helper keeps
/// `.map_err(to_mcp_err)?` out of `.map_err(to_mcp_err)?` shadow territory.
fn to_mcp_err(e: Error) -> McpError {
    mcp_internal(e.to_string())
}

fn mcp_internal(msg: String) -> McpError {
    McpError::internal_error(msg, None)
}

fn mcp_invalid(msg: String) -> McpError {
    McpError::invalid_params(msg, None)
}

fn mcp_not_found(msg: String) -> McpError {
    McpError::invalid_params(msg, None)
}

fn to_ref(n: &HeadingNode) -> NeighborRef {
    NeighborRef {
        node_id: n.id.0,
        level: n.level,
        title: n.title.clone(),
        path: n.path.0.clone(),
    }
}

/// Split `path/to/file.md#Heading > Sub` into (`path/to/file.md`, heading path).
fn parse_qualified_path(s: &str) -> (&str, Option<Vec<String>>) {
    match s.find('#') {
        Some(pos) => {
            let rel = &s[..pos];
            let heading = &s[pos + 1..];
            let segs: Vec<String> = heading
                .split('>')
                .map(|seg| seg.trim().to_string())
                .filter(|seg| !seg.is_empty())
                .collect();
            if segs.is_empty() {
                (rel, None)
            } else {
                (rel, Some(segs))
            }
        }
        None => (s, None),
    }
}
