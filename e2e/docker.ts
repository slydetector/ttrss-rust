import { spawn } from 'node:child_process';

export interface DockerOpts {
  env?: NodeJS.ProcessEnv;
  cwd?: string;
}

// Runs `docker …`, streaming stdout+stderr to the terminal/CI as they
// arrive (a cold compose build takes minutes — buffered output reads as a
// hang) and resolving with the full captured output for parsing (the
// star-banner admin password).
export async function dockerRun(args: string[], opts: DockerOpts = {}): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = spawn('docker', args, {
      env: { ...process.env, ...opts.env },
      cwd: opts.cwd,
    });
    let out = '';
    for (const stream of [child.stdout, child.stderr]) {
      stream.on('data', (d: Buffer) => {
        out += d;
        process.stdout.write(d);
      });
    }
    child.on('close', (code) =>
      // The reason is in the output — a bare "exited 1" is a dead end.
      code === 0 ? resolve(out) : reject(new Error(`docker ${args.join(' ')} exited ${code}\n${out.slice(-2000)}`)),
    );
  });
}
