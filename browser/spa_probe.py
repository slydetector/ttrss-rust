#!/usr/bin/env python3
"""SPA probe — drive the ttrss web SPA in a real browser against the seeded
oracle server, capturing everything a headless curl cannot see:

  * console messages (all levels)          -> out/console.log
  * uncaught JS exceptions (e.g. the old
    `labels.map is not a function`)        -> out/errors.log
  * every backend.php / public.php XHR with
    status + body                          -> out/xhr.jsonl
  * screenshots at each step               -> out/*.png

This is a *spike / debugging harness*, not a pass/fail suite: it drives a fixed
journey (login -> feeds -> prefs tabs) and dumps a structured report so
Rust-vs-PHP divergences surface with the browser as ground truth.

Usage:
    python3 browser/spa_probe.py [--base URL] [--login U] [--password P]
                                 [--headed] [--out DIR] [--settle MS]

Assumes the Rust server is already running (e.g. `make run`, port 9000).
"""

import argparse
import json
from contextlib import suppress
from pathlib import Path
from urllib.parse import parse_qs, urlparse

from playwright.sync_api import sync_playwright

DEFAULT_BASE = "http://127.0.0.1:9000/tt-rss/"


def op_method(url: str) -> str:
    """Extract `op`/`method` (or `op=login`) from a backend/public URL for the
    report, so XHR lines read like `200  Feeds.view` instead of raw query."""
    q = parse_qs(urlparse(url).query)
    op = q.get("op", [""])[0]
    meth = q.get("method", [""])[0]
    if op and meth:
        return f"{op}.{meth}"
    return op or "?"


class Probe:
    def __init__(self, out: Path, settle_ms: int):
        self.out = out
        self.settle_ms = settle_ms
        self.console = []
        self.page_errors = []
        self.xhr = []
        self.responses = []  # (status, url) for *all* responses
        self._console_f = (out / "console.log").open("w")
        self._err_f = (out / "errors.log").open("w")
        self._xhr_f = (out / "xhr.jsonl").open("w")
        self._resp_f = (out / "responses.log").open("w")

    def on_console(self, msg):
        self.console.append((msg.type, msg.text))
        self._console_f.write(f"[{msg.type}] {msg.text}\n")
        self._console_f.flush()

    def on_pageerror(self, err):
        self.page_errors.append(str(err))
        self._err_f.write(f"PAGE ERROR: {err}\n")
        self._err_f.flush()

    def on_response(self, resp):
        url = resp.url
        self.responses.append((resp.status, url))
        self._resp_f.write(f"{resp.status}  {url}\n")
        self._resp_f.flush()
        if "backend.php" in url or "public.php" in url:
            body = ""
            try:
                body = resp.text()
            except Exception as e:  # noqa: BLE001 - capture-and-continue
                body = f"<unreadable: {e}>"
            rec = {
                "url": url,
                "op": op_method(url),
                "status": resp.status,
                "body": body[:4000],
            }
            self.xhr.append(rec)
            self._xhr_f.write(json.dumps(rec) + "\n")
            self._xhr_f.flush()

    def settle(self, page):
        """Wait for the Dojo SPA to quiet down (network idle + fixed grace)."""
        # Dojo may poll indefinitely; if networkidle never arrives, fall back to
        # the fixed grace below.
        with suppress(Exception):
            page.wait_for_load_state("networkidle", timeout=8000)
        page.wait_for_timeout(self.settle_ms)

    def shot(self, page, name):
        page.screenshot(path=str(self.out / f"{name}.png"), full_page=False)
        body_class = ""
        with suppress(Exception):
            body_class = page.evaluate(
                "() => (document.body ? document.body.className : '')"
            )
        print(
            f"  [shot] {name}.png  ({page.url})  title={page.title()!r}  body.class={body_class!r}"
        )

    def click_first(self, page, selectors):
        """Best-effort click of the first selector that matches; returns the
        selector used, or None if none matched."""
        for sel in selectors:
            loc = page.locator(sel)
            if loc.count() == 0:
                continue
            # Present-but-not-actionable elements are still reported as clicked.
            with suppress(Exception):
                loc.first.click(timeout=3000)
            return sel
        return None

    def close(self):
        for f in (self._console_f, self._err_f, self._xhr_f, self._resp_f):
            with suppress(Exception):
                f.close()

    def report(self, base):
        print("\n" + "=" * 68)
        print(f"SPA PROBE REPORT   base={base}")
        print("=" * 68)

        print("\n--- PAGE ERRORS (uncaught JS exceptions) ---")
        if self.page_errors:
            for e in self.page_errors:
                print("  !!", e)
        else:
            print("  (none)")

        print("\n--- CONSOLE (error / warning) ---")
        errs = [(lvl, t) for lvl, t in self.console if lvl in ("error", "warning")]
        if errs:
            for lvl, t in errs:
                print(f"  [{lvl}] {t}")
        else:
            print("  (none)")

        print("\n--- backend.php / public.php XHRs ---")
        if self.xhr:
            for r in self.xhr:
                body = " ".join(r["body"].split())
                flag = ""
                if "E_UNAUTHORIZED" in r["body"] or r["status"] >= 400:
                    flag = "  <<< PROBLEM"
                print(f"  {r['status']}  {r['op']:<22} {body[:90]}{flag}")
        else:
            print("  (none captured)")

        print("\n--- artifacts ---")
        for f in sorted(self.out.iterdir()):
            print(f"  {f}")
        print("=" * 68)


def main():
    ap = argparse.ArgumentParser(description="Drive the ttrss SPA in a real browser.")
    ap.add_argument(
        "--base", default=DEFAULT_BASE, help="app base URL (normalized to end with /)"
    )
    ap.add_argument("--login", default="admin")
    ap.add_argument("--password", default="admin")
    ap.add_argument("--headed", action="store_true", help="show the browser window")
    ap.add_argument("--out", default="browser/out", help="artifact directory")
    ap.add_argument(
        "--settle",
        type=int,
        default=1800,
        help="extra ms to wait for the SPA to settle",
    )
    args = ap.parse_args()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    run = Probe(out, args.settle)
    # The SPA is a base-path app: relative asset URLs (lib/dojo/…, js/…,
    # public.php) only resolve when the page URL ends with a slash. Normalize.
    base = args.base if args.base.endswith("/") else args.base + "/"

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=not args.headed)
        ctx = browser.new_context(viewport={"width": 1280, "height": 900})
        page = ctx.new_page()
        page.on("console", run.on_console)
        page.on("pageerror", run.on_pageerror)
        page.on("response", run.on_response)

        try:
            # 1. Login
            print("== step 1: login ==")
            page.goto(base, wait_until="domcontentloaded", timeout=30000)
            run.settle(page)
            run.shot(page, "01-login")
            # Dojo renders the form asynchronously (body.css_loading hides the
            # DOM until init completes), so the raw <input> is not reliably
            # visible to fill(). Set the values directly and submit natively —
            # this POSTs to public.php?return=... exactly like the real form.
            page.evaluate(
                """(creds) => {
                    const set = (name, val) => {
                        const el = document.querySelector('input[name=' + name + ']');
                        if (el) {
                            el.value = val;
                            el.dispatchEvent(new Event('input', {bubbles: true}));
                        }
                    };
                    set('login', creds.login);
                    set('password', creds.password);
                    const form = document.querySelector('form');
                    if (form) form.submit();
                }""",
                {"login": args.login, "password": args.password},
            )
            run.settle(page)
            run.shot(page, "02-logged-in")
            if (
                "login" in urlparse(page.url).path.lower()
                or "Authentication" in page.title()
            ):
                print("  !! still on login page — login may have failed")

            # 2. Feeds (index.php) — feed tree + first headlines load
            print("== step 2: feeds (index.php) ==")
            page.goto(base + "index.php", wait_until="domcontentloaded", timeout=30000)
            run.settle(page)
            # best-effort: click the first feed to trigger a headlines load
            clicked = run.click_first(
                page,
                (
                    "#feedtree a",
                    "#feedList a",
                    "a.feed",
                    "#feedTree a",
                    "div.feed_list a",
                ),
            )
            if clicked:
                print(f"  clicked feed via {clicked!r}")
            run.settle(page)
            run.shot(page, "03-feeds" + ("-clicked" if clicked else ""))

            # 3. Preferences (prefs.php) — activate the default tab
            print("== step 3: prefs (prefs.php) ==")
            page.goto(base + "prefs.php", wait_until="domcontentloaded", timeout=30000)
            run.settle(page)
            run.shot(page, "04-prefs")

            run.report(base)
        finally:
            browser.close()
            run.close()


if __name__ == "__main__":
    main()
