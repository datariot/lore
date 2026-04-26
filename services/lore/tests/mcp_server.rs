//! End-to-end: spin up the Lore MCP server on a loopback port, speak to it
//! over Streamable HTTP with raw JSON-RPC, and verify every tool returns
//! what we expect for the fixture corpus.
//!
//! We do not depend on the rmcp client to keep this test focused on the wire
//! protocol: any compliant MCP client that sends JSON-RPC over HTTP should
//! get the same results.

use std::fs;
use std::path::Path;
use std::time::Duration;

use lore_service::{CorpusRegistry, IndexOptions, ServeOptions, index_command, serve_http};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::net::TcpListener;

const FIXTURE: &str = "tests/fixtures/mini-kb";

fn copy_fixture_to(dest: &Path) {
    fn copy_dir(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if from.is_dir() {
                copy_dir(&from, &to);
            } else {
                fs::copy(&from, &to).unwrap();
            }
        }
    }
    copy_dir(Path::new(FIXTURE), dest);
}

async fn free_port() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    addr
}

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: Value,
    session: &Option<String>,
) -> (Value, Option<String>) {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let mut req = client
        .post(url)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .json(&body);
    if let Some(sid) = session {
        req = req.header("mcp-session-id", sid);
    }
    let resp = req.send().await.expect("send rpc");
    let next_session = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| session.clone());
    let text = resp.text().await.unwrap();

    // Streamable HTTP returns either plain JSON or an SSE stream. Each SSE
    // frame is a sequence of `field: value` lines; `data:` holds the JSON.
    // Concatenate every `data:` line in order (a single logical frame may
    // span several lines), ignoring other fields like `event:` / `id:`.
    let json_payload = if text.contains("data:") {
        let mut collected = String::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                collected.push_str(rest.trim_start());
            }
        }
        collected
    } else {
        text
    };
    let parsed: Value = serde_json::from_str(&json_payload)
        .unwrap_or_else(|e| panic!("parse json ({e}): {json_payload}"));
    (parsed, next_session)
}

#[tokio::test]
async fn mcp_server_end_to_end() {
    // Build index for fixture corpus.
    let dir = tempdir().unwrap();
    copy_fixture_to(dir.path());
    index_command(IndexOptions::new(dir.path())).unwrap();

    // Load into registry and start server.
    let registry = CorpusRegistry::new();
    registry.load_from_root(dir.path()).unwrap();

    let addr = free_port().await;
    let opts = ServeOptions {
        bind: addr,
        path: "/mcp".to_string(),
    };
    let server = tokio::spawn(async move {
        serve_http(registry, opts).await.ok();
    });

    // Wait a moment for bind.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let url = format!("http://{addr}/mcp");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let session: Option<String> = None;

    // 1) initialize
    let (init, session) = rpc(
        &client,
        &url,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "lore-test", "version": "0.0.0"}
        }),
        &session,
    )
    .await;
    let server_info = &init["result"]["serverInfo"];
    assert_eq!(server_info["name"], "lore");
    assert!(session.is_some(), "session id must be returned");

    // initialized notification (no response expected)
    let body = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .header("mcp-session-id", session.as_deref().unwrap())
        .json(&body)
        .send()
        .await
        .unwrap();

    // 2) tools/list — sanity check schemas surface.
    let (listed, session) = rpc(&client, &url, "tools/list", json!({}), &session).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "list_sources",
        "list_documents",
        "table_of_contents",
        "get_section",
        "search",
        "add_source",
        "backlinks",
        "recent_hot",
        "neighbors",
        "get_by_path",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }

    // 3) list_sources
    let (src_resp, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({"name": "list_sources", "arguments": {}}),
        &session,
    )
    .await;
    let structured = &src_resp["result"]["structuredContent"];
    assert_eq!(structured["sources"][0]["documents"], 3);

    // 4) table_of_contents with depth cap
    let (toc_resp, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "table_of_contents",
            "arguments": {
                "source_id": dir.path().file_name().unwrap().to_str().unwrap(),
                "max_depth": 2
            }
        }),
        &session,
    )
    .await;
    let docs = &toc_resp["result"]["structuredContent"]["documents"];
    assert!(!docs.as_array().unwrap().is_empty());
    for doc in docs.as_array().unwrap() {
        for entry in doc["entries"].as_array().unwrap() {
            assert!(entry["level"].as_u64().unwrap() <= 2);
        }
    }
    let total_docs = docs.as_array().unwrap().len();
    assert!(total_docs >= 3, "fixture has 3 documents");

    // 4b) table_of_contents with path_prefix — only documents under
    // `docs/` should come back. README.md at the root must be excluded.
    let (toc_filtered, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "table_of_contents",
            "arguments": {
                "source_id": dir.path().file_name().unwrap().to_str().unwrap(),
                "path_prefix": "docs/"
            }
        }),
        &session,
    )
    .await;
    let filtered_docs = toc_filtered["result"]["structuredContent"]["documents"]
        .as_array()
        .unwrap();
    assert!(
        !filtered_docs.is_empty() && filtered_docs.len() < total_docs,
        "path_prefix should narrow the result set",
    );
    for d in filtered_docs {
        let rp = d["rel_path"].as_str().unwrap();
        assert!(rp.starts_with("docs/"), "leaked non-matching doc: {rp}");
    }

    // 5) get_section by heading_path
    let (sec_resp, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "get_section",
            "arguments": {
                "source_id": dir.path().file_name().unwrap().to_str().unwrap(),
                "rel_path": "docs/intro.md",
                "heading_path": ["Introduction", "Purpose"]
            }
        }),
        &session,
    )
    .await;
    let body = sec_resp["result"]["structuredContent"]["content"]
        .as_str()
        .unwrap();
    assert!(body.starts_with("## Purpose"));

    // 6) search
    let source_id = dir
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let (search_resp, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "search",
            "arguments": {
                "source_id": source_id,
                "query": "architecture"
            }
        }),
        &session,
    )
    .await;
    let hits = &search_resp["result"]["structuredContent"]["hits"];
    assert!(!hits.as_array().unwrap().is_empty());
    // BM25 should put the `# Architecture` heading first.
    assert_eq!(hits[0]["level"], 1);
    assert_eq!(hits[0]["path"][0], "Architecture");

    // 7) backlinks — the README has `[[architecture]]`, so the Architecture
    // section's document stem should appear as a backlink target.
    let (bl_resp, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "backlinks",
            "arguments": {"source_id": source_id, "target": "architecture"}
        }),
        &session,
    )
    .await;
    let bls = &bl_resp["result"]["structuredContent"]["backlinks"];
    assert!(!bls.as_array().unwrap().is_empty());
    let bare_count = bls.as_array().unwrap().len();

    // 7b) backlinks canonicalization — querying with the qualified path,
    // the `.md` extension, or a `#fragment` should return the same set as
    // the bare basename. The lookup must run `canonical_link_keys` on the
    // query (not just lowercase) so all spellings of one logical target
    // resolve identically.
    for variant in [
        "docs/architecture",
        "docs/architecture.md",
        "Architecture",
        "architecture#Components",
    ] {
        let (resp, sess) = rpc(
            &client,
            &url,
            "tools/call",
            json!({
                "name": "backlinks",
                "arguments": {"source_id": source_id, "target": variant}
            }),
            &session,
        )
        .await;
        let _ = sess;
        let v = &resp["result"]["structuredContent"]["backlinks"];
        let count = v.as_array().expect("backlinks array").len();
        assert_eq!(
            count, bare_count,
            "variant {variant:?} returned {count} backlinks, expected {bare_count} (canonicalized match with bare target)"
        );
    }

    // 8) neighbors — siblings/children of `Introduction > Purpose`
    let (nb_resp, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "neighbors",
            "arguments": {
                "source_id": source_id,
                "rel_path": "docs/intro.md",
                "heading_path": ["Introduction", "Purpose"]
            }
        }),
        &session,
    )
    .await;
    let nb_struct = &nb_resp["result"]["structuredContent"];
    assert_eq!(nb_struct["parent"]["title"], "Introduction");
    let children = nb_struct["children"].as_array().unwrap();
    assert!(children.iter().any(|c| c["title"] == "Why"));

    // 8b) list_documents — no filter returns every doc; path_prefix narrows;
    // frontmatter filters match scalars and array elements identically.
    let (all_docs, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "list_documents",
            "arguments": {"source_id": source_id, "include_frontmatter": true}
        }),
        &session,
    )
    .await;
    let all = all_docs["result"]["structuredContent"]["documents"]
        .as_array()
        .unwrap();
    assert_eq!(all.len(), 3, "fixture has 3 documents");
    assert!(all.iter().any(|d| d["frontmatter"].is_object()));

    let (under_docs, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "list_documents",
            "arguments": {"source_id": source_id, "path_prefix": "docs/"}
        }),
        &session,
    )
    .await;
    let under = under_docs["result"]["structuredContent"]["documents"]
        .as_array()
        .unwrap();
    assert_eq!(under.len(), 2);
    for d in under {
        assert!(d["rel_path"].as_str().unwrap().starts_with("docs/"));
    }

    let (tagged, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "list_documents",
            "arguments": {
                "source_id": source_id,
                "frontmatter": {"tags": "test-fixture"}
            }
        }),
        &session,
    )
    .await;
    let tagged_docs = tagged["result"]["structuredContent"]["documents"]
        .as_array()
        .unwrap();
    assert_eq!(
        tagged_docs.len(),
        1,
        "tags array element match should pick exactly README.md"
    );
    assert_eq!(tagged_docs[0]["rel_path"], "README.md");

    let (titled, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "list_documents",
            "arguments": {
                "source_id": source_id,
                "frontmatter": {"title": "Mini KB"}
            }
        }),
        &session,
    )
    .await;
    let titled_docs = titled["result"]["structuredContent"]["documents"]
        .as_array()
        .unwrap();
    assert_eq!(titled_docs.len(), 1);
    assert_eq!(titled_docs[0]["rel_path"], "README.md");

    let (none, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "list_documents",
            "arguments": {
                "source_id": source_id,
                "frontmatter": {"missing-key": "anything"}
            }
        }),
        &session,
    )
    .await;
    assert_eq!(
        none["result"]["structuredContent"]["documents"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // 9) get_by_path using qualified form.
    let (by_path_resp, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "get_by_path",
            "arguments": {
                "source_id": source_id,
                "qualified_path": "docs/intro.md#Introduction > Purpose"
            }
        }),
        &session,
    )
    .await;
    let content = by_path_resp["result"]["structuredContent"]["content"]
        .as_str()
        .unwrap();
    assert!(content.starts_with("## Purpose"));

    // 10) recent_hot — `get_section` and `get_by_path` should have bumped
    // the Purpose node's access count twice.
    let (hot_resp, _session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({
            "name": "recent_hot",
            "arguments": {"source_id": source_id, "limit": 5}
        }),
        &session,
    )
    .await;
    let hot_nodes = hot_resp["result"]["structuredContent"]["nodes"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        hot_nodes
            .iter()
            .any(|n| n["path"][1] == "Purpose" && n["access_count"].as_u64().unwrap() >= 2)
    );

    server.abort();
}
