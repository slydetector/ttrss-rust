# Driving & debugging a live app through MCP

When you need **UX-level truth** — what's rendered, what an XHR actually returned,
what the JS threw — the raw-HTTP diff isn't enough. The surface is the live page.

**MCP is what makes this dynamic.** The `mcp_chrome_*` tools hold a stateful handle
on the real page (a11y tree, in-page JS, network log, console). That turns
debugging into an **observe → act → observe** loop: each step reacts to what the
page *actually* shows, so you can chase a bug to root cause or walk a flow that'd
be painful to pre-script. It's the opposite of a fixed test — no selectors or
wait-logic to get right up front, and you adapt in real time.

## The loop

1. `navigate_page` to the view.
2. `take_snapshot` → an a11y tree with stable per-node `uid`s. **Act by `uid`**
   (`click`/`fill`/`hover`), not by CSS selector — widgets rewrite the DOM, uids
   don't go stale.
3. Read ground truth: `list_network_requests` + `get_network_request` (request and
   **response bodies**), `list_console_messages` (incl. page errors), and
   `evaluate_script` for ad-hoc DOM queries.
4. Repeat.

## Habits that pay off

- **The response *body* is the source of truth, not the HTTP status.** App-level
  errors are usually a 200 + error payload, and a "success" dialog proves nothing
  — read the XHR body.
- **Enumerate the surface, then diff it.** List what the app *will* request (DOM
  `href`/`url` loads, JS call literals, form fields) and compare against what the
  backend *handles*. The gap *is* the bug — found without clicking every path.
  `browser/diff_rpc_surface.py` automates this for the RPC surface.
- **Re-snapshot after any navigation/state change** — uids are per-snapshot.

## Reproducible capture

For before/after diffs or CI, a small Playwright harness (see
`browser/spa_probe.py`) drives a fixed journey and dumps console / page-errors /
every XHR (status + body) / screenshots. **MCP for exploration, the probe for
repeatable evidence.**
