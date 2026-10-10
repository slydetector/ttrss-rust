import { promises as fs } from 'node:fs';
import path from 'node:path';
import { expect, test, type Locator, type Page } from '@playwright/test';

const ADMIN_PASS_FILE = path.join(__dirname, '.admin-pass');

// Breadcrumb state for the (single) test in this file — the afterEach hook
// reads these when a step fails before the step-local attachments ran.
const pageErrors: string[] = [];
const consoleLog: string[] = [];

/**
 * Subscribe to a feed through the real UI (Actions... → "Subscribe to feed..."
 * → URL → Subscribe) and return the feed's tree row.
 *
 * Success signal: the dialog destroys itself on hide (a failed subscribe
 * shows an error and stays open). Only then does Feeds.reload() re-render the
 * tree: the new feed sits inside the collapsed "Uncategorized" category. We
 * poll — auto-expanding (the expando span is display:none in this UI, and the
 * reload can land after a manual expand) — until the row shows up.
 *
 * The row is matched by `data-feed-id > 0`, NOT by title: the reference
 * `Feeds::_subscribe` inserts the feed with title `[Unknown]` and resolves
 * the real title in `update_basic_info` (a second fetch in the same
 * request) — a flaky fetch leaves `[Unknown]` until the daemon's next update,
 * and the tree only re-renders on reload. A fresh install has no other real
 * feeds, so the first positive id is unambiguous. The caller asserts the
 * resolved title once the daemon's update has landed (the unread poll).
 */
async function subscribeFeed(page: Page, url: string): Promise<{ row: Locator; feedId: number }> {
  await page.locator('[title="Actions..."]').click();
  await page.getByText('Subscribe to feed...').click();
  await page.locator('#feedDlg_feedUrl').fill(url);
  // dijit.form.Button renders as <span class="dijitButton"> — the page has
  // no <button> elements, so role-based locators can't reach it.
  await page.locator('.dijitButton', { hasText: 'Subscribe' }).click();

  await page.locator('.dijitDialog').waitFor({ state: 'detached', timeout: 30_000 });
  try {
    await page.waitForFunction(
      () => {
        const rows = [...document.querySelectorAll('#feedTree [data-feed-id]')];
        if (rows.some((r) => Number(r.getAttribute('data-feed-id')) > 0)) return true;
        const uncat = rows.find(
          (r) => r.getAttribute('data-is-cat') === 'true' && r.textContent?.includes('Uncategorized'),
        );
        const expando = uncat?.querySelector('.dijitTreeExpando');
        if (uncat && expando && expando.classList.contains('dijitTreeExpandoClosed'))
          (expando as HTMLElement).click();
        return false;
      },
      undefined,
      { timeout: 30_000, polling: 1000 },
    );
  } catch {
    // The row never showed up — dump the tree's actual state so the failure
    // is diagnosable from the CI log (screenshot/trace live in the container).
    const dump = await page.evaluate(() => {
      const rows = [...document.querySelectorAll('#feedTree [data-feed-id]')].map((r) => ({
        id: r.getAttribute('data-feed-id'),
        cat: r.getAttribute('data-is-cat'),
        text: r.textContent?.trim().slice(0, 80),
      }));
      return { rows, html: document.getElementById('feedTree')?.outerHTML?.slice(0, 1500) ?? '' };
    });
    console.log(`FEEDTREE-DUMP ${JSON.stringify(dump)}`);
    throw new Error(`no real feed row (data-feed-id > 0) appeared; rows: ${JSON.stringify(dump.rows)}`);
  }
  // Keyed by feed id: stable across the reloads the caller performs.
  const feedId = await page.evaluate(() => {
    const r = [...document.querySelectorAll('#feedTree [data-feed-id]')].find(
      (r) => Number(r.getAttribute('data-feed-id')) > 0,
    );
    return r ? Number(r.getAttribute('data-feed-id')) : -1;
  });
  return { row: page.locator(`#feedTree [data-feed-id="${feedId}"]`), feedId };
}

// The golden path for a fresh install: one real user session. Steps are
// added incrementally (boot first); each is a test.step so the Playwright
// report/trace pinpoints the failing area even though this is one test.
// The feed tree rows render asynchronously after load (Feeds.reload), and
// feeds land inside collapsed categories, so rows must be waited for /
// expanded, never assumed present. Row counters refresh on a 60 s UI ticker
// (App.updateRuntimeInfo) — assert structure, never exact counts.
// A healthy run is ~10 s of browser work; 90 s caps any hang in the
// browser-interacting steps (the config-level timeout covers the stack
// setup/teardown tests, which legitimately take longer).
test('fresh install golden path', { timeout: 90_000 }, async ({ page }, testInfo) => {
  // Error policy: uncaught exceptions fail the run at the step where they
  // occur; console messages are breadcrumbs — attached to the report always
  // (not just on failure), never gating (legacy Dojo noise should not sink
  // the gate). Module-scoped (single test in this file) so the failure hook
  // below can still reach them.
  page.on('pageerror', (err) => pageErrors.push(String(err)));
  page.on('console', (msg) => consoleLog.push(`[${msg.type()}] ${msg.text()}`));
  const attachConsole = async () => {
    if (consoleLog.length > 0)
      await testInfo.attach('browser-console', {
        body: consoleLog.join('\n'),
        contentType: 'text/plain',
      });
  };

  await test.step('boot', async () => {
    const res = await page.goto('/tt-rss/');
    expect(res?.status()).toBe(200);
    // The login screen renders — the JS bundle loaded and the page template
    // resolved in a real browser.
    await expect(page.locator('body.ttrss_login')).toBeVisible();
    expect(pageErrors, `uncaught page errors: ${pageErrors.join(' | ')}`).toEqual([]);
  });

  await test.step('login', async () => {
    // Fresh install ⇒ the star banner's random password (captured by setup).
    const password = (await fs.readFile(ADMIN_PASS_FILE, 'utf8')).trim();
    // include/login_form.php: #login / input[name=password], POST op=login.
    await page.locator('#login').fill('admin');
    await page.locator('input[name="password"]').fill(password);
    await page.getByRole('button', { name: 'Log in' }).click();
    // index.php: the main UI body; the DiJit feed tree widget booted.
    await expect(page.locator('body.ttrss_main')).toBeVisible();
    await expect(page.locator('#feedTree')).toBeVisible();
    expect(pageErrors, `uncaught page errors: ${pageErrors.join(' | ')}`).toEqual([]);
  });

  await test.step('subscribe & read', async () => {
    // Subscribe only validates the URL (articles are NOT stored at subscribe
    // time) — the in-container feed daemon (5 s tick in this test; stock 120 s)
    // fetches the feed on its own. No manual update.
    const { row: hn, feedId } = await subscribeFeed(page, 'https://hnrss.org/frontpage');

    // A live session's tree counters refresh on the UI's 60 s ticker, but a
    // fresh page load renders them straight from the server — so poll by
    // reloading (the session cookie survives) until the unread counter is
    // non-empty: the daemon's update has landed. The read settles 2 s after
    // load: the tree rows render server-side, but their counters are filled
    // by the post-load getfeedtree XHR.
        await expect
      .poll(
        () =>
          page.reload().then(() =>
            page.evaluate(
              (id) =>
                new Promise<number>((res) =>
                  setTimeout(() => {
                    const row = document.querySelector(`#feedTree [data-feed-id="${id}"]`);
                    const c = row?.querySelector('.counterNode.unread')?.textContent?.trim() ?? '';
                    res(c === '' ? 0 : Number(c));
                  }, 2000),
                ),
              feedId,
            ),
          ),
        { timeout: 60_000, intervals: [2_000] },
      )
      .toBeGreaterThan(0);

    // The daemon's update landed (unread > 0), so the feed's title is
    // resolved in the DB and the tree — re-rendered by the poll's last
    // reload — shows it. This also covers the flaky-subscribe case where
    // `update_basic_info`'s fetch failed and the row briefly read [Unknown].
    await expect(hn).toContainText('Hacker News');

    // Open the feed: the row click deep-links (#f=<id>&c=0) and the
    // headlines load into #headlines-frame (data-headlines-count > 0).
    await hn.click();
    await page.waitForFunction(
      () => {
        const f = document.getElementById('headlines-frame');
        return f !== null && Number(f.getAttribute('data-headlines-count') || 0) > 0;
      },
      undefined,
      { timeout: 30_000 },
    );

    // Read an article in-app: JS-click the title wrap (a real click lands on
    // the <a target="_blank"> and opens an external tab; the inline
    // onclick=Headlines.click is the in-app path).
    const first = page.locator('#headlines-frame .cdm').first();
    const title = (await first.getAttribute('data-article-title')) ?? '';
    await first.locator('.titleWrap').evaluate((e) => (e as HTMLElement).click());
    // The combined-mode content pane renders the article (title + body).
    await expect(page.locator('#content-wrap')).toContainText(title);
    expect(pageErrors, `uncaught page errors: ${pageErrors.join(' | ')}`).toEqual([]);
  });

  // Breadcrumb: attach the full console transcript (the trace records it too).
  await attachConsole();
  const consoleErrors = consoleLog.filter((l) => l.startsWith('[error]'));
  if (consoleErrors.length > 0) {
    console.log(`console.error (non-fatal): ${consoleErrors.join(' | ')}`);
  }
});

// On a mid-test failure the step-local attachments never ran — keep the
// breadcrumbs in the report so the run is diagnosable without the container.
test.afterEach(async ({}, testInfo) => {
  if (testInfo.status === 'passed') return;
  await testInfo.attach('browser-console (on failure)', {
    body: consoleLog.join('\n'),
    contentType: 'text/plain',
  });
  if (pageErrors.length > 0) {
    await testInfo.attach('page-errors (on failure)', {
      body: pageErrors.join('\n'),
      contentType: 'text/plain',
    });
  }
  console.log(`PAGE-ERRORS (on failure): ${pageErrors.join(' | ') || '(none)'}`);
  const tail = consoleLog.slice(-40).join('\n');
  console.log(`CONSOLE-TAIL (on failure):\n${tail}`);
});
