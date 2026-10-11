import { test as teardown } from '@playwright/test';
import { promises as fs } from 'node:fs';
import path from 'node:path';

import { dockerRun } from './docker';

const ROOT = path.resolve(__dirname, '..');
// The SYNTHESIZED pair setup created (project ttrss-e2e on :19000) — not the
// blessed docker/ one, whose .env the checkout may not have.
const COMPOSE = 'e2e/compose.yaml';
const PROJECT = 'ttrss-e2e';
const ADMIN_PASS_FILE = path.join(__dirname, '.admin-pass');

teardown('tear down the e2e stack', async () => {
  // State + log tails — `down` below deletes the containers and their
  // logs, so capture first. ps says which containers ever ran (an
  // app that was Created-but-never-started shows no logs at all); the
  // db tail shows a stuck postgres gate; the app tail (RUST_LOG=debug
  // in CI) carries the daemon's fetch/update detail.
  for (const [label, args] of [
    ['state', ['ps']],
    ['db log (tail 100)', ['logs', '--tail', '100', 'db']],
    ['app log (tail 300)', ['logs', '--tail', '300', 'app']],
  ] as const) {
    console.log(`--- e2e ${label} ---`);
    // dockerRun streams to stdout as it runs; on a failure just note it.
    await dockerRun(['compose', '-f', COMPOSE, '-p', PROJECT, ...args], { cwd: ROOT }).catch((e: Error) =>
      console.log(String(e)),
    );
  }

  if (process.env.E2E_KEEP) {
    console.log('E2E_KEEP set — stack left running (ttrss-e2e)');
    return;
  }

  await dockerRun(['compose', '-f', COMPOSE, '-p', PROJECT, 'down', '-v', '--remove-orphans'], { cwd: ROOT });

  // Remove the throwaway artifacts; setup synthesizes them next run. The
  // captured password is a secret (regenerates on the next fresh boot).
  await fs.rm(path.join(__dirname, 'compose.yaml'), { force: true });
  await fs.rm(path.join(__dirname, '.env'), { force: true });
  await fs.rm(ADMIN_PASS_FILE, { force: true });
});
