import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

test('OMP wrapper injects TPS, restores profiles, and isolates its update target', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-omp-wrapper-'));
  const fakeOmp = path.join(directory, 'omp');
  const observedArguments = path.join(directory, 'arguments.json');
  const wrapper = fileURLToPath(new URL('../omp-with-tps.mjs', import.meta.url));
  const configRoot = path.join(directory, 'config');
  const isolatedEnv = {
    ...process.env,
    PI_CONFIG_DIR: path.relative(homedir(), configRoot),
    XDG_STATE_HOME: '',
    HERDR_TPS_OMP_BIN: fakeOmp,
    HERDR_TPS_TEST_ARGUMENTS: observedArguments,
  };

  await writeFile(
    fakeOmp,
    `#!/usr/bin/env node
import { writeFileSync } from 'node:fs';
writeFileSync(process.env.HERDR_TPS_TEST_ARGUMENTS, JSON.stringify({
  args: process.argv.slice(2),
  path: process.env.PATH,
}));
`,
    'utf8',
  );
  await chmod(fakeOmp, 0o755);

  try {
    const restoredSession =
      '/home/user/.omp/profiles/pro2/agent/sessions/project/restored-session.jsonl';
    const result = spawnSync(process.execPath, [wrapper, `--resume=${restoredSession}`], {
      encoding: 'utf8',
      env: isolatedEnv,
    });

    assert.equal(result.status, 0, result.stderr);
    const observed = JSON.parse(await readFile(observedArguments, 'utf8'));
    const args = observed.args;
    assert.equal(args[0], '--extension');
    assert.equal(args[1], fileURLToPath(new URL('../omp-extension.mjs', import.meta.url)));
    assert.deepEqual(args.slice(2), ['--profile=pro2', `--resume=${restoredSession}`]);
    assert.equal(observed.path.split(path.delimiter)[0], directory);
    assert.equal(
      await realpath(path.join(configRoot, 'profiles/pro2/run/daemons')),
      await realpath(path.join(configRoot, 'run/daemons')),
    );

    const explicitProfileResult = spawnSync(
      process.execPath,
      [wrapper, '--profile=pro2', '--resume=/tmp/pro2-session.jsonl'],
      {
        encoding: 'utf8',
        env: isolatedEnv,
      },
    );
    assert.equal(explicitProfileResult.status, 0, explicitProfileResult.stderr);
    const explicitProfileArguments = JSON.parse(await readFile(observedArguments, 'utf8')).args;
    assert.deepEqual(explicitProfileArguments.slice(2), [
      '--profile=pro2',
      '--resume=/tmp/pro2-session.jsonl',
    ]);
  } finally {
    await rm(directory, { recursive: true });
  }
});

test('OMP wrapper preserves live pro2 daemons and does not spawn another client', async (t) => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-omp-sharing-block-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const clients = path.join(directory, 'profiles/pro2/run/daemons/scope/clients');
  await mkdir(clients, { recursive: true });
  await writeFile(path.join(clients, 'active.json'), JSON.stringify({ pid: process.pid }));
  const result = spawnSync(
    process.execPath,
    [fileURLToPath(new URL('../omp-with-tps.mjs', import.meta.url)), '--profile=pro2'],
    {
      encoding: 'utf8',
      env: {
        ...process.env,
        PI_CONFIG_DIR: path.relative(homedir(), directory),
        XDG_STATE_HOME: '',
        HERDR_TPS_OMP_BIN: process.execPath,
      },
    },
  );
  assert.equal(result.status, 2);
  assert.match(result.stderr, /请先正常退出所有旧 omp2 会话/);
  assert.equal(await realpath(clients), clients);
});

test('OMP wrapper forwards SIGTERM and waits for child cleanup', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-omp-signal-'));
  const fakeOmp = path.join(directory, 'omp');
  const stopped = path.join(directory, 'stopped');
  let wrapperChild;
  let childPid;
  await writeFile(
    fakeOmp,
    `#!/usr/bin/env node
import { writeFileSync } from 'node:fs';
process.once('SIGTERM', () => { writeFileSync(process.env.STOPPED, 'yes'); process.exit(0); });
console.log(process.pid);
setInterval(() => {}, 1000);
`,
  );
  await chmod(fakeOmp, 0o755);
  try {
    const result = await new Promise((resolve, reject) => {
      wrapperChild = spawn(
        process.execPath,
        [fileURLToPath(new URL('../omp-with-tps.mjs', import.meta.url))],
        {
          env: { ...process.env, HERDR_TPS_OMP_BIN: fakeOmp, STOPPED: stopped },
          stdio: ['ignore', 'pipe', 'pipe'],
        },
      );
      const timer = setTimeout(() => reject(new Error('OMP wrapper shutdown timed out')), 3_000);
      wrapperChild.stdout.once('data', (data) => {
        childPid = Number(data.toString().trim());
        wrapperChild.kill('SIGTERM');
      });
      wrapperChild.once('error', (error) => {
        clearTimeout(timer);
        reject(error);
      });
      wrapperChild.once('exit', (code) => {
        clearTimeout(timer);
        resolve(code);
      });
    });
    assert.equal(result, 0);
    assert.equal(await readFile(stopped, 'utf8'), 'yes');
  } finally {
    if (wrapperChild?.exitCode === null && wrapperChild.signalCode === null)
      wrapperChild.kill('SIGKILL');
    if (childPid) {
      try {
        process.kill(childPid, 'SIGKILL');
      } catch {
        /* Already exited. */
      }
    }
    await rm(directory, { recursive: true });
  }
});
