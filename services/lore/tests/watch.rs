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

/// Poll `table_of_contents` until the corpus reports exactly `target`
/// documents, or the timeout elapses. Returns whether the count arrived.
async fn wait_for_doc_count(
    client: &reqwest::Client,
    url: &str,
    source_id: &str,
    session: &Option<String>,
    target: usize,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let (toc, _s) = rpc(
            client,
            url,
            "tools/call",
            json!({"name": "table_of_contents", "arguments": {"source_id": source_id}}),
            session,
        )
        .await;
        if let Some(list) = toc["result"]["structuredContent"]["documents"].as_array()
            && list.len() == target
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Every `rel_path` the corpus currently reports.
async fn doc_paths(
    client: &reqwest::Client,
    url: &str,
    source_id: &str,
    session: &Option<String>,
) -> Vec<String> {
    let (toc, _s) = rpc(
        client,
        url,
        "tools/call",
        json!({"name": "table_of_contents", "arguments": {"source_id": source_id}}),
        session,
    )
    .await;
    toc["result"]["structuredContent"]["documents"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|d| d["rel_path"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Poll until `pred` holds over the corpus's `rel_path` set, then return that
/// set. Returns the last observation on timeout so the caller can report what
/// it actually saw rather than just "didn't happen".
async fn wait_for_paths(
    client: &reqwest::Client,
    url: &str,
    source_id: &str,
    session: &Option<String>,
    pred: impl Fn(&[String]) -> bool,
    timeout: Duration,
) -> Result<Vec<String>, Vec<String>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = Vec::new();
    while std::time::Instant::now() < deadline {
        last = doc_paths(client, url, source_id, session).await;
        if pred(&last) {
            return Ok(last);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last)
}

/// Collect every heading title in a nested `roots[].children[]` TOC.
fn collect_titles(entries: &[Value], out: &mut Vec<String>) {
    for e in entries {
        if let Some(t) = e["title"].as_str() {
            out.push(t.to_string());
        }
        if let Some(children) = e["children"].as_array() {
            collect_titles(children, out);
        }
    }
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
    // Seeded before indexing so the ignore rules are in force from the start;
    // step 8 writes files under them and expects the watcher to skip both.
    fs::write(root.join(".gitignore"), "secret.md\n").unwrap();
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

    // 3b) Prime the FSEvents stream before the fixed-deadline assertions
    // below (KB-640). On macOS an event that fires during the watcher's
    // kqueue registration window can be *dropped outright*, not merely
    // delayed — so polling for a single write can hang forever if that
    // first write was the dropped one. We instead re-touch a throwaway
    // sentinel until the watcher reports it; each rewrite is a fresh event
    // that lands once the stream is warm. Content varies per iteration so a
    // hash-dedup reindex can't skip it. Write + delete leave the baseline
    // untouched, so every real assertion afterward measures a warm stream.
    let warmup = root.join("warmup.md");
    let mut warmed = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut attempt = 0u32;
    while std::time::Instant::now() < deadline {
        attempt += 1;
        fs::write(&warmup, format!("# Warmup {attempt}\n")).unwrap();
        if wait_for_doc_count(
            &client,
            &url,
            &source_id,
            &session,
            2,
            Duration::from_secs(1),
        )
        .await
        {
            warmed = true;
            break;
        }
    }
    assert!(
        warmed,
        "watcher never delivered a warmup file in 30s — FSEvents may be unavailable (sandboxed runner?)"
    );
    fs::remove_file(&warmup).unwrap();
    assert!(
        wait_for_doc_count(
            &client,
            &url,
            &source_id,
            &session,
            1,
            Duration::from_secs(10)
        )
        .await,
        "watcher did not observe the warmup deletion"
    );

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
    assert_eq!(docs[0]["roots"].as_array().unwrap().len(), 1);

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
        let roots = toc_now["result"]["structuredContent"]["documents"][0]["roots"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut titles: Vec<String> = Vec::new();
        collect_titles(&roots, &mut titles);
        if titles.len() >= 3 {
            updated = true;
            assert!(titles.iter().any(|t| t == "Added"));
            assert!(titles.iter().any(|t| t == "Deeper"));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(updated, "modified document did not re-index within 5s");

    // 8) The watcher must apply the same rules the indexer did. Write two
    // files it should skip — one under a hidden directory (the `git worktree
    // add` shape that doubled a vault), one matching .gitignore — plus a
    // canary it *should* pick up. Waiting on the canary is what makes the
    // negative assertion meaningful: once the canary has landed, the watcher
    // has demonstrably drained past the excluded writes, so their absence is
    // a decision rather than a race.
    fs::create_dir_all(root.join(".claude/worktrees/wt")).unwrap();
    fs::write(root.join(".claude/worktrees/wt/dup.md"), "# Dup\n").unwrap();
    fs::write(root.join("secret.md"), "# Secret\n").unwrap();
    fs::write(root.join("canary.md"), "# Canary\n").unwrap();

    // Wait on the canary *by path*, not by document count: if the watcher is
    // wrongly admitting the excluded files the count overshoots, and a
    // count-based wait would fail with a misleading "canary never arrived".
    let paths = wait_for_paths(
        &client,
        &url,
        &source_id,
        &session,
        |p| p.iter().any(|x| x == "canary.md"),
        Duration::from_secs(10),
    )
    .await
    .unwrap_or_else(|last| {
        panic!("canary never arrived — cannot judge the excluded files. saw: {last:?}")
    });

    assert!(
        !paths.iter().any(|p| p.contains("worktrees")),
        "watcher indexed a file under a hidden directory: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "secret.md"),
        "watcher indexed a gitignored file: {paths:?}"
    );

    fs::remove_file(root.join("canary.md")).unwrap();
    wait_for_paths(
        &client,
        &url,
        &source_id,
        &session,
        |p| !p.iter().any(|x| x == "canary.md"),
        Duration::from_secs(10),
    )
    .await
    .unwrap_or_else(|last| panic!("canary did not drop from the corpus. saw: {last:?}"));

    // 9) Delete a file and verify it vanishes.
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
