# e2e — Playwright end-to-end suite

Drives the blessed Docker stack from a real browser: build the image, boot a
fresh install, and walk the golden user path (boot → login → subscribe →
the in-container daemon fetches the feed on its own → open the feed and read
an article). Chromium only.

## Run

```sh
make e2e          # headless (the CI entry; DEBUG=pw:api breadcrumbs)
make e2e-headed   # visible browser, for debugging

# raw (first time: cd e2e && npm ci && npx playwright install chromium)
cd e2e && npx playwright test
```

`E2E_KEEP=1 npx playwright test` leaves the stack running afterwards
(`ttrss-e2e` project, port **19000** — never the shadow's 9000) for manual
poking; the captured admin password stays in `.admin-pass`.

## Layout

| file | role |
|---|---|
| `playwright.config.ts` | projects: `stack-setup` → (chromium spec) → `stack-cleanup` |
| `global.setup.ts` | synthesize throwaway files → `compose build` → `up -d` → readiness → star-banner password capture → HEALTHCHECK `healthy` |
| `global.teardown.ts` | `down -v --remove-orphans` → delete throwaway files + `.admin-pass` (skipped with `E2E_KEEP`) |
| `e2e.spec.ts` | the golden-path test (boot → login → subscribe & read) |

Gitignored, regenerated per run: `compose.yaml`, `.env`, `.admin-pass`,
`test-results/`, `node_modules/`.

## Quirk: `e2e/compose.yaml` and `e2e/.env` are synthesized copies

The blessed `docker/compose.yaml` references `.env` **relative to its own
directory** (`env_file: - .env`), and a missing env file is a hard compose
error. Rather than have the harness write a hidden file into `docker/`, setup
copies `docker/compose.yaml` and `docker/.env.example` **verbatim** into
`e2e/` and runs compose against the copy, so `env_file` resolves to
`e2e/.env`.

Why a verbatim copy works without edits:

- the compose file is **depth-invariant** — `e2e/` and `docker/` are both
  directly under the repo root, so `build: context: ..` reaches the same root
  `Dockerfile` from either;
- all volumes are named (no relative bind paths to re-root);
- `-p ttrss-e2e` overrides the file's `name: ttrss`;
- the host port arrives via shell env (`HTTP_PORT=19000`), which beats the
  file in `${...}` interpolation.

Consequences:

- the copies are **throwaway**: regenerated at the top of every run (always
  stock values) and deleted by teardown. There is deliberately **no drift
  check** between the copy and the blessed file — with a per-run regeneration
  there is nothing to keep in sync. If you need e2e-specific environment, set
  it in the shell (it wins over `.env`).
- `docker/compose.yaml` remains the **single source of truth** for the stack.
  If it ever stops being depth-invariant (e.g. a relative bind mount is added),
  the copy must start carrying a transform — do that in `global.setup.ts`
  step 1 and keep this section honest.

## CI

Runs in the `build` job, after `cargo test` (`.github/workflows/build.yml`) —
the Rust suite is the primary gate, the golden path is the final end-to-end
proof. Two runner-specific facts not obvious from the file (recorded
2026-10-10):

- **No service-container healthchecks, no dind.** This act-based runner kills
  the job on the *first* failed service probe (it ignores `--health-retries`),
  and stock dind's healthcheck has a 1 s probe cap that never passes there.
  So the e2e stack runs in the **runner's own docker daemon** (a
  `/var/run/docker.sock` bind mount — the runner's `valid_volumes` must allow
  it) and readiness is polled by the `stack-setup` project step.
- **Reaching the app.** Published ports land on the runner host; the browser
  (inside the job container) reaches port 19000 over the job network's
  **gateway IP** — the workflow step exports it as `TTRSS_E2E_HOST` (the
  config's `baseURL` default is `127.0.0.1` for local runs).

## Failure breadcrumbs

On a spec failure the run emits, in the CI log: `FEEDTREE-DUMP` (the feed
tree's actual DOM rows + an HTML slice, when the subscribed feed's row never
appears), `PAGE-ERRORS` (uncaught page exceptions) and `CONSOLE-TAIL` (last
40 console lines); the console transcript + page errors are also attached to
the report (`browser-console (on failure)`). Per-test trace + video land in
`test-results/` — inside the container, so they're only reachable locally.
