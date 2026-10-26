import { test as setup } from '@playwright/test';
import { execFile } from 'node:child_process';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';

import { dockerRun } from './docker';

const pExec = promisify(execFile);

const ROOT = path.resolve(__dirname, '..'); // repo root
const BLESSED_COMPOSE = path.join(ROOT, 'docker/compose.yaml'); // source of truth
const ENV_EXAMPLE = path.join(ROOT, 'docker/.env.example');
const COMPOSE = 'e2e/compose.yaml'; // synthesized each run; deleted by teardown
const ENV_FILE = path.join(__dirname, '.env'); // ditto (stock values)
const PROJECT = 'ttrss-e2e';
const PORT = 19000; // never the shadow's 9000
// Host the app's published port lives on: 127.0.0.1 locally; in CI the
// suite's docker stack runs in a dind sidecar, so the host is the service
// alias (TTRSS_E2E_HOST=docker).
const HOST = process.env.TTRSS_E2E_HOST ?? '127.0.0.1';
const BASE = `http://${HOST}:${PORT}/tt-rss`;
const ADMIN_PASS_FILE = path.join(__dirname, '.admin-pass');

// The blessed stack is configured via docker/.env; the host port is
// overridden here (shell env beats the .env file in compose interpolation).
async function dockerCompose(...args: string[]): Promise<string> {
  return dockerRun(['compose', '-f', COMPOSE, '-p', PROJECT, ...args], {
    cwd: ROOT,
    env: { HTTP_PORT: String(PORT) },
  });
}

// Elapsed-seconds breadcrumb — a stalled fixture should say WHERE it
// stalled, not sit silent for minutes.
const T0 = Date.now();
const elapsed = () => `+${((Date.now() - T0) / 1000).toFixed(0)}s`;

setup('bring up the fresh e2e stack', async () => {
  // 1. Synthesize the throwaway compose/env pair. Verbatim copies: the
  //    blessed compose is depth-invariant (context: .. reaches the root
  //    Dockerfile from e2e/ too; all volumes are named; -p overrides the
  //    file's name:), and env_file: .env now resolves to e2e/.env.
  //    Regenerated every run ⇒ stock values, zero drift to police.
  await fs.copyFile(BLESSED_COMPOSE, path.join(__dirname, 'compose.yaml'));
  await fs.copyFile(ENV_EXAMPLE, ENV_FILE);
  // Test-only knobs (not part of the blessed stock): tick the in-container
  // feed daemon every 5 s (stock is 120 s) so the golden-path subscribe
  // settles in seconds instead of minutes; and log at debug so a failure's
  // app log carries the daemon/update detail.
  // html5ever logs every parsed HTML token, sqlx::query every SQL
  // statement, selectors::matching every CSS match — all three swallow
  // the 300-line log tail the teardown prints, so keep them off (the
  // rest stays at debug).
  await fs.appendFile(
    ENV_FILE,
    '\nTTRSS_DAEMON_SLEEP_INTERVAL=5\nRUST_LOG=debug,html5ever=off,sqlx::query=off,selectors::matching=off\n',
  );

  // 2. Kill any leftover stack (a previous E2E_KEEP run) so this is always
  //    a fresh install — the golden path depends on first-boot state
  //    (star banner, empty feed list).
  await dockerCompose('down', '-v', '--remove-orphans').catch(() => {});

  // 3. Build the image — unless it is already in the daemon. CI builds it
  //    with `docker buildx build --load` (docker-container builder; the
  //    daemon's buildkit store stays empty), so an unconditional `compose
  //    build` would recompile everything from scratch; local dev has no
  //    image on first use and builds it here.
  const imageInDaemon = await dockerRun(['image', 'inspect', 'ghcr.io/slydetector/ttrss-rust:latest'])
    .then(() => true)
    .catch(() => false);
  if (imageInDaemon) {
    console.log(`${elapsed()} image ghcr.io/slydetector/ttrss-rust:latest in daemon — skipping build`);
  } else {
    console.log(`${elapsed()} building image…`);
    await dockerCompose('build', '--provenance=false', '--sbom=false');
    console.log(`${elapsed()} build done`);
  }

  // 4. Up — fresh named volumes by construction (down -v above).
  await dockerCompose('up', '-d');
  console.log(`${elapsed()} stack up — waiting for ${BASE}/index.php (≤180s)`);

  // 5. Readiness: the login page answers 200.
  const deadline = Date.now() + 180_000;
  let ready = false;
  let ticks = 0;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${BASE}/index.php`);
      if (res.status === 200) {
        ready = true;
        break;
      }
    } catch {
      // connection refused while the app boots (chown + bootstrap)
    }
    // Heartbeat every ~20 s: a cold postgres gate + slow app boot can take
    // a while, and silence reads as a hang.
    if (++ticks % 10 === 0) console.log(`${elapsed()} …still not answering`);
    await new Promise((r) => setTimeout(r, 2000));
  }
  if (!ready) throw new Error(`app not ready at ${BASE}/index.php within 180s`);
  console.log(`${elapsed()} app ready`);

  // 6. Fresh install ⇒ the star banner printed a random admin password.
  const logs = await dockerCompose('logs', 'app');
  const match = logs.match(/Setting initial built-in admin user password to '([^']+)'/);
  if (!match) throw new Error('star-banner admin password not found in app logs');
  await fs.writeFile(ADMIN_PASS_FILE, match[1], { mode: 0o600 });

  // 7. The image's own HEALTHCHECK reports healthy. The probe runs on a
  //    30s interval, so this is a poll, not a one-shot: the app serving
  //    (step 4) and the probe certifying are two different clocks.
  console.log(`${elapsed()} waiting for HEALTHCHECK (≤120s, 30s probe interval)`);
  const healthDeadline = Date.now() + 120_000;
  let status = '';
  let hticks = 0;
  while (Date.now() < healthDeadline) {
    const health = await pExec('docker', [
      'inspect',
      '--format',
      '{{.State.Health.Status}}',
      `${PROJECT}-app-1`,
    ]);
    status = health.stdout.trim();
    if (status === 'healthy') break;
    if (status === 'unhealthy') break;
    if (++hticks % 6 === 0) console.log(`${elapsed()} …health: ${status || 'starting'}`);
    await new Promise((r) => setTimeout(r, 5000));
  }
  if (status !== 'healthy') throw new Error(`HEALTHCHECK status: ${status} (want healthy)`);
  console.log(`${elapsed()} healthy`);

  console.log(`stack ready: ${BASE} — admin password captured to e2e/.admin-pass`);
});
