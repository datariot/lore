# Running Lore as a daily driver (macOS)

How the author's machine runs Lore continuously against a live Obsidian
vault, serving Claude Code sessions. Set up 2026-07-19 per KB-643.

## Install

```bash
cargo install --path services/lore     # → ~/.cargo/bin/lore
```

## Index the corpus

Add `.lore/` to the corpus repo's `.gitignore` first — the index is a
build artifact, not content.

```bash
lore index ~/Workspace/knowledge-base
# indexed 1136 files (15927 headings) in 1141ms (write 13ms)
```

## LaunchAgent

`~/Library/LaunchAgents/io.datariot.lore.plist` runs `lore watch` with
`RunAtLoad` + `KeepAlive`, logging to `~/Library/Logs/lore.log`:

```xml
<key>ProgramArguments</key>
<array>
    <string>/Users/datariot/.cargo/bin/lore</string>
    <string>watch</string>
    <string>-r</string>
    <string>/Users/datariot/Workspace/knowledge-base</string>
</array>
```

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.datariot.lore.plist
launchctl kickstart -k gui/$(id -u)/io.datariot.lore   # restart after a rebuild
```

Additional corpora are additional `-r` flags. Each root must be indexed
once with `lore index` before it can be served.

A watched root may safely be a repository root. Both the initial walk and
the watcher skip hidden directories, `.gitignore`d paths, and `.lore/`, so
build output and `.claude/worktrees/` never enter the corpus. Editing a
`.gitignore` while the watcher runs re-arms it for *future* events; documents
already in the index are only re-evaluated by rebuilding the source
(`add_source` with `rebuild: true`, or `lore index` + restart).

## Register with Claude Code

```bash
claude mcp add --scope user --transport http lore http://127.0.0.1:7331/mcp
claude mcp list        # lore: ... - ✔ Connected
```

## Verifying

- **Health**: `claude mcp list` must say `✔ Connected`. "Connected ·
  tools fetch failed" means the client completed `initialize` but
  refused the tool list — historically an MCP protocol-version
  mismatch (see below).
- **Watcher**: write a file with a nonsense token into the corpus, then
  `search` for it. Observed latency from write to searchable: ~2 s
  (250 ms debounce + full derived-index rebuild over 15,927 nodes).
- **Logs**: `tail -f ~/Library/Logs/lore.log`.

## Gotchas

- **Protocol-version negotiation is invisible to our integration
  tests.** `tests/mcp_server.rs` speaks raw JSON-RPC, so it happily
  talks whatever version the server offers. rmcp 0.8.5 pinned its
  `initialize` reply to `2025-03-26` and Claude Code silently refused
  to fetch tools; the fix was upgrading to rmcp 2.x. When adopting a
  new client, verify with that client — not just the suite.
- **`lore watch` holds the index in memory**; a rebuilt binary needs
  `launchctl kickstart -k`, not just `cargo install`.
- The agent runs unsandboxed under launchd, so FSEvents delivery works
  (unlike the sandboxed test environment — see CLAUDE.md gotchas).
