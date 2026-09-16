import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { shareOmpDaemons } from '../share-omp-daemons.mjs';

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'omp-share-daemons-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const source = path.join(root, 'profiles/pro2/run/daemons');
  const target = path.join(root, 'run/daemons');
  await mkdir(source, { recursive: true });
  await mkdir(target, { recursive: true });
  return { root, source, target };
}

test('shares daemon state, preserves profile state and archives the old runtime', async (t) => {
  const { root, source, target } = await fixture(t);
  const agent = path.join(root, 'profiles/pro2/agent');
  await mkdir(agent);
  await writeFile(path.join(agent, 'config.yml'), 'model: pro2-model\n');
  await writeFile(path.join(source, 'old-log'), 'old');
  await writeFile(path.join(target, 'shared-log'), 'shared');
  assert.equal((await shareOmpDaemons({ configRoot: root })).status, 'ready');
  assert.equal(await realpath(source), source);
  const result = await shareOmpDaemons({ configRoot: root, apply: true });
  assert.equal(result.status, 'shared');
  assert.equal(await realpath(source), await realpath(target));
  assert.equal(await readFile(path.join(source, 'shared-log'), 'utf8'), 'shared');
  assert.equal(await readFile(path.join(result.backup, 'old-log'), 'utf8'), 'old');
  assert.equal(await readFile(path.join(agent, 'config.yml'), 'utf8'), 'model: pro2-model\n');
  assert.equal((await shareOmpDaemons({ configRoot: root, apply: true })).status, 'already-shared');
});

for (const file of [
  'scope/broker.pid',
  'scope/clients/client.json',
  'scope/daemons/browser/meta.json',
]) {
  test(`refuses to replace a live runtime found in ${file}`, async (t) => {
    const { root, source } = await fixture(t);
    const record = file.endsWith('meta.json')
      ? { daemon: { pid: process.pid } }
      : { pid: process.pid };
    await mkdir(path.dirname(path.join(source, file)), { recursive: true });
    await writeFile(path.join(source, file), JSON.stringify(record));
    const result = await shareOmpDaemons({ configRoot: root, apply: true });
    assert.equal(result.status, 'blocked-live-runtime');
    assert.deepEqual(result.pids, [process.pid]);
    assert.equal(await realpath(source), source);
  });
}

test('refuses a conflicting link instead of replacing it', async (t) => {
  const { root, source } = await fixture(t);
  await rm(source, { recursive: true });
  await symlink(root, source, 'dir');
  await assert.rejects(shareOmpDaemons({ configRoot: root, apply: true }), /points elsewhere/);
  assert.equal(await realpath(source), root);
});

test('concurrent first launches cannot archive each other’s shared directory', async (t) => {
  const { root, source, target } = await fixture(t);
  const results = await Promise.allSettled([
    shareOmpDaemons({ configRoot: root, apply: true }),
    shareOmpDaemons({ configRoot: root, apply: true }),
  ]);
  assert.ok(
    results.some((result) => result.status === 'fulfilled' && result.value.status === 'shared'),
  );
  assert.equal(await realpath(source), await realpath(target));
  assert.equal((await shareOmpDaemons({ configRoot: root, apply: true })).status, 'already-shared');
});
