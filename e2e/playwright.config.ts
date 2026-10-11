import { defineConfig } from '@playwright/test';

// The e2e suite: stack-setup (bring up the fresh stack) → chromium (the
// golden-path spec) → stack-cleanup.
export default defineConfig({
  testDir: '.',
  timeout: 10 * 60 * 1000, // worst cold run ≈ 6 min (build ~4-5 min on CI
  // + postgres init + app boot + 30 s health probe); green runs exit the
  // fixture in ~2 min — the cap only bounds a stuck bring-up
  workers: 1,
  // Breadcrumbs by default: a hang or failure should leave a full trace
  // (DOM snapshots, network, console), a video, and an HTML report —
  // plus DEBUG=pw:api on the Makefile target logs every Playwright call
  // to the terminal, so a hang shows its stuck call live.
  reporter: [['list'], ['html', { open: 'never' }]],
  use: {
    trace: 'on',
    video: 'on',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'stack-setup',
      testMatch: /global\.setup\.ts/,
      teardown: 'stack-cleanup',
    },
    {
      name: 'stack-cleanup',
      testMatch: /global\.teardown\.ts/,
    },
    {
      name: 'chromium',
      use: {
        browserName: 'chromium',
        // Origin only: an absolute path in goto() (e.g. '/') resolves
        // against the origin and would silently drop a path prefix here.
        // e2e port; never 9000. Host: 127.0.0.1 locally, the dind service
        // alias in CI (TTRSS_E2E_HOST) — see global.setup.ts.
        baseURL: `http://${process.env.TTRSS_E2E_HOST ?? '127.0.0.1'}:19000`,
      },
      dependencies: ['stack-setup'],
      testMatch: /e2e\.spec\.ts/,
    },
  ],
});
