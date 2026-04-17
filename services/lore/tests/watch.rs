//! End-to-end: start a watched Lore server, modify a file on disk, and
//! verify the MCP surface reflects the change within a bounded window.

use std::fs;
use std::time::Duration;

use lore_service::{
    CorpusRegistry, IndexOptions, ServeOptions, index_command, run_watcher, serve_http,
};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::net::TcpListener;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watch_triggers_incremental_reindex() {
    // 1) Seed corpus on disk and build initial index.
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("initial.md"), "# Initial\n\nhello\n").unwrap();
    index_command(IndexOptions::new(&root)).unwrap();

    // 2) Start server + watcher.
    let registry = CorpusRegistry::new();
    registry.load_from_root(&root).unwrap();

    let addr = free_port().await;
    let reg_for_watch = registry.clone();
    let watcher_task = tokio::spawn(async move {
        // Tight debounce so the test completes quickly.
        run_watcher(reg_for_watch, Duration::from_millis(120))
            .await
            .ok();
    });
    let serve_registry = registry.clone();
    let server = tokio::spawn(async move {
        serve_http(
            serve_registry,
            ServeOptions {
                bind: addr,
                path: "/mcp".to_string(),
            },
        )
        .await
        .ok();
    });
    // Let HTTP bind and the watcher install its kqueue handles.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let url = format!("http://{addr}/mcp");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let session: Option<String> = None;

    // 3) MCP handshake.
    let (_init, session) = rpc(
        &client,
        &url,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "watch-test", "version": "0.0.0"}
        }),
        &session,
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

    let source_id = root.file_name().unwrap().to_str().unwrap().to_string();

    // 4) Confirm baseline TOC has one doc, one root heading.
    let (toc_before, session) = rpc(
        &client,
        &url,
        "tools/call",
        json!({"name": "table_of_contents", "arguments": {"source_id": source_id}}),
        &session,
    )
    .await;
    let docs = &toc_before["result"]["structuredContent"]["documents"];
    assert_eq!(docs.as_array().unwrap().len(), 1);
    assert_eq!(docs[0]["entries"].as_array().unwrap().len(), 1);

    // 5) Create a NEW file in the watched root.
    fs::write(root.join("second.md"), "# Second\n\n## Nested\n").unwrap();

    // 6) Poll until the watcher has re-indexed (debounce + filesystem jitter).
    let mut arrived = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let (toc_now, sess) = rpc(
            &client,
            &url,
            "tools/call",
            json!({"name": "table_of_contents", "arguments": {"source_id": source_id}}),
            &session,
        )
        .await;
        let _ = sess;
        let list = &toc_now["result"]["structuredContent"]["documents"];
        if list.as_array().unwrap().len() == 2 {
            arrived = true;
            assert!(
                list.as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["rel_path"] == "second.md")
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        arrived,
        "new document did not appear in TOC within 5s — watcher may not be firing"
    );

    // 7) Modify an existing file, expect heading count to change.
    fs::write(
        root.join("initial.md"),
        "# Initial\n\n## Added\n\n### Deeper\n",
    )
    .unwrap();
    let mut updated = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let (toc_now, _sess) = rpc(
            &client,
            &url,
            "tools/call",
            json!({
                "name": "table_of_contents",
                "arguments": {"source_id": source_id, "rel_path": "initial.md"}
            }),
            &session,
        )
        .await;
        let entries = toc_now["result"]["structuredContent"]["documents"][0]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if entries.len() >= 3 {
            updated = true;
            let titles: Vec<&str> = entries
                .iter()
                .map(|e| e["title"].as_str().unwrap_or(""))
                .collect();
            assert!(titles.contains(&"Added"));
            assert!(titles.contains(&"Deeper"));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(updated, "modified document did not re-index within 5s");

    // 8) Delete a file and verify it vanishes.
    fs::remove_file(root.join("second.md")).unwrap();
    let mut removed = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let (toc_now, _sess) = rpc(
            &client,
            &url,
            "tools/call",
            json!({"name": "table_of_contents", "arguments": {"source_id": source_id}}),
            &session,
        )
        .await;
        let list = &toc_now["result"]["structuredContent"]["documents"];
        if list.as_array().unwrap().len() == 1 {
            removed = true;
            assert_eq!(list[0]["rel_path"], "initial.md");
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(removed, "deleted document did not drop from TOC within 5s");

    server.abort();
    watcher_task.abort();
}
