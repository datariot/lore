//! The read tools accept the request shapes callers actually send.
//!
//! Every case here is a shape lifted from a Claude Code transcript that the
//! server used to refuse with `failed to deserialize parameters` — eleven of
//! thirteen failed lore calls in a month, the same few guesses made
//! independently by different sessions. Driven over real HTTP against the
//! fixture corpus, as `mcp_server.rs` does, so the wire is what is tested.

use std::fs;
use std::path::Path;
use std::time::Duration;

use lore_service::{CorpusRegistry, IndexOptions, ServeOptions, index_command, serve_http};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{Value, json};
use tempfile::tempdir;

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
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap()
}

/// One JSON-RPC round trip; unwraps the SSE framing when the server uses it.
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
    let payload = if text.contains("data:") {
        text.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<String>()
    } else {
        text
    };
    let parsed: Value =
        serde_json::from_str(&payload).unwrap_or_else(|e| panic!("parse json ({e}): {payload}"));
    (parsed, next_session)
}

/// Index each root, load them all into one registry, serve, and complete the
/// MCP handshake. Returns the URL and an initialised session.
async fn serve(roots: &[&Path]) -> (reqwest::Client, String, Option<String>) {
    let registry = CorpusRegistry::new();
    for root in roots {
        index_command(IndexOptions::new(root)).unwrap();
        registry.load_from_root(root).unwrap();
    }
    let addr = free_port().await;
    let opts = ServeOptions {
        bind: addr,
        path: "/mcp".to_string(),
    };
    tokio::spawn(async move {
        serve_http(registry, opts).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let url = format!("http://{addr}/mcp");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let (_init, session) = rpc(
        &client,
        &url,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "lore-test", "version": "0.0.0"}
        }),
        &None,
    )
    .await;
    client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .header("mcp-session-id", session.as_deref().unwrap())
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    (client, url, session)
}

async fn call(
    client: &reqwest::Client,
    url: &str,
    session: &Option<String>,
    name: &str,
    arguments: Value,
) -> Value {
    let (resp, _) = rpc(
        client,
        url,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
        session,
    )
    .await;
    resp
}

fn structured(resp: &Value) -> &Value {
    assert!(
        resp["result"]["isError"] != true,
        "tool call failed: {}",
        resp["result"]["content"]
    );
    &resp["result"]["structuredContent"]
}

#[tokio::test]
async fn with_one_corpus_loaded_no_call_needs_a_source_id() {
    let dir = tempdir().unwrap();
    copy_fixture_to(dir.path());
    let (client, url, session) = serve(&[dir.path()]).await;
    let source_id = dir.path().file_name().unwrap().to_str().unwrap();

    // `search` with only a query — the most common refused shape.
    let found = call(
        &client,
        &url,
        &session,
        "search",
        json!({"query": "purpose"}),
    )
    .await;
    let found = structured(&found);
    assert_eq!(
        found["source_id"], source_id,
        "one corpus: the response names it"
    );
    assert_eq!(found["sources"], json!([source_id]));
    let hit = &found["hits"][0];
    assert_eq!(hit["source_id"], source_id, "every hit names its corpus");

    // `get_section` by the ids the hit carries, exactly as returned.
    let sec = call(
        &client,
        &url,
        &session,
        "get_section",
        json!({"doc_id": hit["doc_id"], "node_id": hit["node_id"]}),
    )
    .await;
    let sec = structured(&sec);
    assert_eq!(sec["rel_path"], hit["rel_path"]);
    assert_eq!(sec["node_id"], hit["node_id"]);
    assert!(
        sec["content"].as_str().unwrap().starts_with('#'),
        "a section starts at its heading"
    );

    // `get_by_path` under the family name for a path, with no `#`: the
    // whole document, which the description always promised.
    let whole = call(
        &client,
        &url,
        &session,
        "get_by_path",
        json!({"rel_path": "docs/intro.md"}),
    )
    .await;
    let whole = structured(&whole);
    let text = whole["content"].as_str().unwrap();
    let on_disk = fs::read_to_string(dir.path().join("docs/intro.md")).unwrap();
    assert_eq!(text, on_disk, "no heading named means the whole file");
    assert_eq!(whole["heading_path"], json!([]));
    assert_eq!(whole["byte_range"][1], on_disk.len());

    // The same through `get_section` with only a `rel_path`.
    let whole2 = call(
        &client,
        &url,
        &session,
        "get_section",
        json!({"rel_path": "docs/intro.md"}),
    )
    .await;
    assert_eq!(structured(&whole2)["content"], whole["content"]);
}

#[tokio::test]
async fn with_several_corpora_search_spans_them_and_reads_resolve_by_path() {
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    copy_fixture_to(a.path());
    copy_fixture_to(b.path());
    // Give corpus B one document A does not have, so a path can pick it.
    fs::write(
        b.path().join("docs/only-in-b.md"),
        "# Only In B\n\nA heading found nowhere else.\n",
    )
    .unwrap();
    let (client, url, session) = serve(&[a.path(), b.path()]).await;
    let id_a = a.path().file_name().unwrap().to_str().unwrap().to_string();
    let id_b = b.path().file_name().unwrap().to_str().unwrap().to_string();

    // Unqualified search covers both; each hit says where it came from.
    let found = call(
        &client,
        &url,
        &session,
        "search",
        json!({"query": "purpose"}),
    )
    .await;
    let found = structured(&found);
    assert!(
        found["source_id"].is_null(),
        "several corpora: no single source"
    );
    let mut sources: Vec<&str> = found["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    sources.sort();
    let mut expected = vec![id_a.as_str(), id_b.as_str()];
    expected.sort();
    assert_eq!(sources, expected);
    let hit_sources: std::collections::HashSet<&str> = found["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["source_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        hit_sources.len(),
        2,
        "identical corpora: hits from both, ranked together"
    );

    // A term only B holds is `full` coverage for the union, not partial.
    let only_b = call(
        &client,
        &url,
        &session,
        "search",
        json!({"query": "nowhere"}),
    )
    .await;
    let only_b = structured(&only_b);
    assert_eq!(only_b["coverage"]["level"], "full");
    assert_eq!(only_b["hits"][0]["source_id"], id_b);

    // A path that lives in exactly one corpus needs no source_id...
    let sec = call(
        &client,
        &url,
        &session,
        "get_by_path",
        json!({"qualified_path": "docs/only-in-b.md#Only In B"}),
    )
    .await;
    assert_eq!(structured(&sec)["source_id"], id_b);

    // ...and a path that lives in both is refused with the choice, not guessed.
    let ambiguous = call(
        &client,
        &url,
        &session,
        "get_section",
        json!({"rel_path": "docs/intro.md", "heading_path": ["Introduction", "Purpose"]}),
    )
    .await;
    // An `invalid_params` refusal is a JSON-RPC error, not a tool result.
    let msg = ambiguous["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a refusal, got: {ambiguous}"));
    assert!(
        msg.contains("pass `source_id`"),
        "unexpected refusal: {msg}"
    );
    assert!(
        msg.contains(&id_a) && msg.contains(&id_b),
        "the refusal lists the choices: {msg}"
    );
}
