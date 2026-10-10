import { test as teardown } from '@playwright/test';
import { execFile } from 'node:child_process';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';

const pExec = promisify(execFile);

const ROOT = path.resolve(__dirname, '..');
// The SYNTHESIZED pair setup created (project ttrss-e2e on :19000) — not the
// blessed docker/ one, whose .env the checkout may not have.
const COMPOSE = 'e2e/compose.yaml';
const PROJECT = 'ttrss-e2e';
const ADMIN_PASS_FILE = path.join(__dirname, '.admin-pass');

teardown('tear down the e2e stack', async () => {
  if (process.env.E2E_KEEP) {
    console.log('E2E_KEEP set — stack left running (ttrss-e2e)');
    return;
  }

  await pExec('docker', ['compose', '-f', COMPOSE, '-p', PROJECT, 'down', '-v', '--remove-orphans'], {
    cwd: ROOT,
    maxBuffer: 16 * 1024 * 1024,
  });

  // Remove the throwaway artifacts; setup synthesizes them next run. The
  // captured password is a secret (regenerates on the next fresh boot).
  await fs.rm(path.join(__dirname, 'compose.yaml'), { force: true });
  await fs.rm(path.join(__dirname, '.env'), { force: true });
  await fs.rm(ADMIN_PASS_FILE, { force: true });
});
