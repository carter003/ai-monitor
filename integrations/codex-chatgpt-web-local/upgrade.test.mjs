import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { assertIdle, assertRecoverableWrapper, cliWrapper, options, restore, run, snapshot } from './upgrade.mjs';

const script = join(dirname(fileURLToPath(import.meta.url)), 'upgrade.mjs');
async function scratch(t, cleanups = []) {
  const root = await fs.mkdtemp(join(tmpdir(), 'codex-local-maintenance-test-'));
  // after hooks run FIFO: keep process cleanup and directory removal in one hook.
  t.after(async () => {
    const failures = [];
    for (const cleanup of [...cleanups].reverse()) {
      try { await cleanup(); } catch (error) { failures.push(error); }
    }
    // Preserve PID/diagnostic files if cleanup fails, but still try every child.
    if (failures.length) throw new AggregateError(failures, `Fixture cleanup failed: ${root}`);
    await fs.rm(root, { recursive: true, force: true });
  });
  return root;
}
async function within(promise, milliseconds) {
  let timer;
  try {
    return await Promise.race([promise.then(() => true),
      new Promise(resolve => { timer = setTimeout(() => resolve(false), milliseconds); })]);
  } finally { clearTimeout(timer); }
}
function spawnFixture(cleanups, args, options, grace = 1000) {
  const child = spawn(process.execPath, args, options);
  const closed = new Promise(resolve => child.once('close', resolve));
  const stop = async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
    if (!await within(closed, grace)) {
      child.kill('SIGKILL');
      assert.ok(await within(closed, 1000), `Fixture child ${child.pid} did not close`);
    }
  };
  cleanups.push(stop);
  return { child, closed, stop };
}
async function fixtureReady({ child, closed }, milliseconds = 2000) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), milliseconds);
  try {
    return await Promise.race([
      once(child.stdout, 'data', { signal: controller.signal }).then(([data]) => data),
      once(child, 'error', { signal: controller.signal }).then(([error]) => { throw error; }),
      closed.then(() => { throw new Error(`Fixture child exited before readiness (${child.exitCode ?? child.signalCode})`); })
    ]);
  } finally { clearTimeout(timer); controller.abort(); }
}
async function stopDetachedFixture(pidFile, entry) {
  let identity;
  try { identity = JSON.parse(await fs.readFile(pidFile, 'utf8')); }
  catch (error) { if (error.code === 'ENOENT') return; throw error; }
  const { pid, startTime } = identity;
  assert.ok(Number.isSafeInteger(pid) && pid > 1 && pid !== process.pid && /^\d+$/.test(startTime), 'Invalid fixture PID identity');
  async function sameLiveProcess() {
    const stat = await fs.readFile(`/proc/${pid}/stat`, 'utf8');
    const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
    return fields[19] === startTime && !['Z', 'X'].includes(fields[0]);
  }
  async function running() {
    try {
      if (!await sameLiveProcess()) return false;
      const argv = (await fs.readFile(`/proc/${pid}/cmdline`, 'utf8')).split('\0').filter(Boolean);
      // A fixture exiting after SIGTERM can lose its cmdline between the two proc reads.
      if (!argv.length) {
        await new Promise(resolve => setTimeout(resolve, 20));
        if (!await sameLiveProcess()) return false;
      }
      assert.deepEqual(argv, [process.execPath, entry], 'Refusing to signal a non-fixture process');
      return true;
    } catch (error) { if (['ENOENT', 'ESRCH'].includes(error.code)) return false; throw error; }
  }
  for (const signal of ['SIGTERM', 'SIGKILL']) {
    if (!await running()) return;
    try { process.kill(pid, signal); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    const deadline = performance.now() + 1000;
    do {
      if (!await running()) return;
      await new Promise(resolve => setTimeout(resolve, 20));
    } while (performance.now() < deadline);
  }
  assert.fail(`Detached fixture ${pid} did not exit`);
}
const digest = value => createHash('sha256').update(value).digest('hex');

test('scratch awaits fixture close before removing its directory, including SIGKILL fallback', async t => {
  for (const stubborn of [false, true]) {
    let root, child, output = '';
    await t.test(stubborn ? 'ignores SIGTERM' : 'delayed graceful exit', async scope => {
      const cleanups = [];
      root = await scratch(scope, cleanups);
      const entry = join(root, 'cleanup-fixture.cjs');
      await fs.writeFile(entry, `const fs=require('fs');
process.on('SIGTERM',()=>{${stubborn ? '' : `setTimeout(()=>{console.log(fs.existsSync(${JSON.stringify(root)}));process.exit(0);},40);`}});
setInterval(()=>{},1000);console.log('ready');\n`);
      const fixture = spawnFixture(cleanups, [entry], { stdio: ['ignore', 'pipe', 'inherit'] }, 200);
      ({ child } = fixture);
      child.stdout.on('data', data => { output += data; });
      await fixtureReady(fixture);
    });
    assert.ok(child.exitCode !== null || child.signalCode !== null);
    if (stubborn) assert.equal(child.signalCode, 'SIGKILL');
    else { assert.equal(child.exitCode, 0); assert.match(output, /true/); }
    await assert.rejects(fs.stat(root), { code: 'ENOENT' });
  }
});

test('fixture startup rejects early exit, spawn errors and a readiness timeout', async t => {
  const cleanups = [];
  const root = await scratch(t, cleanups);
  await assert.rejects(fixtureReady(spawnFixture(cleanups, ['-e', 'process.exit(7)'], { stdio: 'pipe' })), /exited before readiness \(7\)/);
  await assert.rejects(fixtureReady(spawnFixture(cleanups, ['-e', ''], { cwd: join(root, 'absent'), stdio: 'pipe' })), { code: 'ENOENT' });
  await assert.rejects(fixtureReady(spawnFixture(cleanups, ['-e', 'setInterval(()=>{},1000)'], { stdio: 'pipe' }), 100), { name: 'AbortError' });
});

test('staging preserves a busy live runtime, can cancel without stopping it, and finalizes only after a new runtime starts', async t => {
  const cleanups = [];
  const root = await scratch(t, cleanups), source = join(root, 'source'), core = join(root, 'core');
  const stateDir = join(root, 'state'), bin = join(root, 'bin'), lib = join(root, 'lib');
  await Promise.all([fs.mkdir(join(source, 'launcher/electron'), { recursive: true }),
    fs.mkdir(stateDir), fs.mkdir(bin), fs.mkdir(lib), fs.mkdir(core)]);
  await fs.writeFile(join(source, 'package.json'), JSON.stringify({ version: '6.1.19', packageManager: 'bun@1.4.0' }));
  await fs.writeFile(join(source, 'launcher/electron/runtime-install.cjs'), `
const fs=require('fs'),path=require('path'),crypto=require('crypto');
exports.validateRuntimeBundle=(root,e)=>{const m=JSON.parse(fs.readFileSync(path.join(root,'manifest.json')));
if(m.appVersion!==e.version||e.bundleId&&m.bundleId!==e.bundleId)throw Error('identity mismatch');
for(const f of m.files)if(crypto.createHash('sha256').update(fs.readFileSync(path.join(root,f.path))).digest('hex')!==f.sha256)throw Error('file hash mismatch');};\n`);
  const git = args => run('git', ['-C', source, ...args], { capture: true });
  await git(['init', '-q']); await git(['add', '.']);
  await git(['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'baseline']);
  await git(['tag', 'v6.1.19']);
  const head = (await git(['rev-parse', 'HEAD'])).trim();
  const patch = join(root, 'fix.patch'); await fs.writeFile(patch, '');
  const runtime = join(core, 'versions', `6.1.19-${process.platform}-${process.arch}`);
  await fs.mkdir(join(runtime, 'app'), { recursive: true }); await fs.mkdir(join(runtime, 'runtime'));
  const entry = join(runtime, 'app/cli.js'), configPath = join(core, 'config.json');
  const daemon = `const fs=require('fs'),http=require('http');const c=fs.existsSync(${JSON.stringify(configPath)})?JSON.parse(fs.readFileSync(${JSON.stringify(configPath)})):{};const s=http.createServer((q,r)=>{r.setHeader('content-type','application/json');r.end(JSON.stringify({status:'ok',service:'codex-chatgpt-web',version:'6.1.19',pid:process.pid,accepting_turns:true,active_http_turns:1,active_browser_turns:0}));});s.listen(c.port||0,'127.0.0.1',()=>console.log(s.address().port));\n`;
  await fs.writeFile(entry, daemon); await fs.writeFile(join(runtime, 'runtime/bun'), 'fixture');
  const manifest = text => { const files = [
    { path: 'app/cli.js', size: Buffer.byteLength(text), sha256: digest(text) },
    { path: 'runtime/bun', size: 7, sha256: digest('fixture') }];
    return { schemaVersion: 2, appVersion: '6.1.19', platform: process.platform, arch: process.arch,
      entrypoint: 'app/cli.js', bundleId: digest(JSON.stringify(files)), files }; };
  const oldManifest = manifest(daemon), newDaemon = `${daemon}// final patched runtime\n`, newManifest = manifest(newDaemon);
  await fs.writeFile(join(runtime, 'manifest.json'), JSON.stringify(oldManifest));
  async function startDaemon() {
    const fixture = spawnFixture(cleanups, [entry], { stdio: ['ignore', 'pipe', 'inherit'] });
    const data = await fixtureReady(fixture);
    return { ...fixture, port: Number(data.toString().trim()) };
  }
  const old = await startDaemon();
  await fs.writeFile(configPath, JSON.stringify({ host: '127.0.0.1', port: old.port,
    releaseVersion: '6.1.19', runtimeCommand: [process.execPath, entry] }));
  const launcher = join(bin, 'codex-web-gpt'), cli = join(bin, 'codex-chatgpt-web');
  await fs.writeFile(launcher, 'old launcher', { mode: 0o755 });
  await fs.writeFile(cli, 'old CLI', { mode: 0o751 });
  await fs.writeFile(join(lib, 'run-appimage'), '#!/bin/sh\nexit 99\n', { mode: 0o755 });
  const artifact = join(root, 'final.AppImage'); await fs.writeFile(artifact, 'verified artifact');
  await fs.writeFile(join(stateDir, 'current.json'), JSON.stringify({ source, version: '6.1.19', head,
    upstreamCommit: head, patchPath: patch, patchSha256: digest(''), sourceDigest: digest(''), verified: { at: 'test' },
    build: { artifact, artifactSha256: digest('verified artifact'), bundleId: newManifest.bundleId, manifest: newManifest } }));
  const audit = join(root, 'systemctl-audit');
  await fs.writeFile(join(bin, 'systemctl'), `#!${process.execPath}\nrequire('fs').writeFileSync(${JSON.stringify(audit)},'CALLED');process.exit(99);\n`, { mode: 0o755 });
  const common = ['--source', source, '--patch', patch, '--state-dir', stateDir,
    '--core-home', core, '--install-root', lib, '--bin-dir', bin];
  const execute = (command, ...args) => run(process.execPath, [script, command, ...common, ...args], {
    capture: true, env: { ...process.env, PATH: `${bin}:${process.env.PATH}` } });
  const health = () => fetch(`http://127.0.0.1:${old.port}/healthz`).then(r => r.json());
  await execute('stage');
  assert.equal((await health()).pid, old.child.pid);
  assert.equal(await fs.readFile(entry, 'utf8'), daemon);
  await assert.rejects(execute('finalize'), /has not started/);
  await assert.rejects(fs.stat(audit), { code: 'ENOENT' });
  assert.equal(JSON.parse(await execute('status')).activation, 'awaiting-user-restart');
  const stagedReceiptPath = JSON.parse(await fs.readFile(join(lib, 'local/active-receipt.json'))).receipt;
  const stagedReceipt = JSON.parse(await fs.readFile(stagedReceiptPath));
  const stagedWrappers = await Promise.all([fs.readFile(launcher, 'utf8'), fs.readFile(cli, 'utf8')]);
  // The SECOND backup must be validated before the first wrapper is restored.
  const cliBackup = stagedReceipt.backups.cli.backup;
  const originalCli = await fs.readFile(cliBackup);
  await fs.writeFile(cliBackup, 'corrupt CLI backup');
  await assert.rejects(execute('rollback'), /Backup drift/);
  assert.deepEqual(await Promise.all([fs.readFile(launcher, 'utf8'), fs.readFile(cli, 'utf8')]), stagedWrappers);
  await assert.rejects(fs.stat(audit), { code: 'ENOENT' });
  await fs.writeFile(cliBackup, originalCli);
  await execute('rollback');
  assert.equal(await fs.readFile(launcher, 'utf8'), 'old launcher');
  assert.equal(await fs.readFile(cli, 'utf8'), 'old CLI');
  assert.equal((await fs.stat(cli)).mode & 0o777, 0o751);
  assert.equal(JSON.parse(await fs.readFile(stagedReceiptPath)).status, 'unstaged');
  await assert.rejects(execute('rollback', '--receipt', stagedReceiptPath), /Receipt already rolled back or staging cancelled/);
  await assert.rejects(fs.stat(audit), { code: 'ENOENT' });
  assert.equal((await health()).pid, old.child.pid);
  await execute('stage');
  await old.stop();
  await fs.writeFile(entry, newDaemon); await fs.writeFile(join(runtime, 'manifest.json'), JSON.stringify(newManifest));
  const next = await startDaemon();
  await execute('finalize');
  const active = JSON.parse(await fs.readFile(join(lib, 'local/active-receipt.json')));
  const receipt = JSON.parse(await fs.readFile(active.receipt));
  assert.equal(receipt.status, 'installed'); assert.equal(receipt.running.bundleId, newManifest.bundleId);
  assert.equal((await health()).pid, next.child.pid);
  await assert.rejects(fs.stat(audit), { code: 'ENOENT' });
});

test('idle gate rejects busy, missing and ambiguous activity evidence', () => {
  const health = { status: 'ok', service: 'codex-chatgpt-web', pid: 123,
    active_http_turns: 0, active_browser_turns: 0 };
  assert.doesNotThrow(() => assertIdle(health));
  for (const change of [{ active_http_turns: 1 }, { active_browser_turns: 1 },
    { active_http_turns: undefined }, { active_browser_turns: '0' }, { pid: 0 }, { status: 'error' }]) {
    assert.throws(() => assertIdle({ ...health, ...change }));
  }
});

test('prepare requires explicit version before mutation and unknown flags fail', async t => {
  const root = await scratch(t);
  await assert.rejects(run(process.execPath, [script, 'prepare', '--state-dir', join(root, 'state')], { capture: true }), /explicit --version/);
  await assert.rejects(fs.stat(join(root, 'state')), { code: 'ENOENT' });
  assert.throws(() => options(['build', '--force', 'yes']), /Expected --option/);
});

test('argv subprocess execution preserves shell metacharacters literally', async t => {
  const root = await scratch(t);
  const marker = join(root, 'must-not-exist');
  const payload = `$(touch ${marker}); echo dangerous`;
  const output = await run(process.execPath, ['-e', 'process.stdout.write(process.argv[1])', payload], { capture: true });
  assert.equal(output, payload);
  await assert.rejects(fs.stat(marker), { code: 'ENOENT' });
});

test('wrapper snapshot/restore preserves symlink target and file mode, refuses corrupt backup', async t => {
  const root = await scratch(t), target = join(root, 'official'), entry = join(root, 'cli');
  await fs.writeFile(target, 'official');
  await fs.symlink(target, entry);
  const savedLink = await snapshot(entry, join(root, 'unused'));
  await fs.unlink(entry);
  await fs.writeFile(entry, 'custom');
  await restore(entry, savedLink);
  assert.equal(await fs.readlink(entry), target);
  assert.equal(await fs.readFile(target, 'utf8'), 'official');
  await fs.unlink(entry);
  await fs.writeFile(entry, 'wrapper', { mode: 0o751 });
  const savedFile = await snapshot(entry, join(root, 'backup'));
  await fs.writeFile(entry, 'replacement');
  await restore(entry, savedFile);
  assert.equal(await fs.readFile(entry, 'utf8'), 'wrapper');
  assert.equal((await fs.stat(entry)).mode & 0o777, 0o751);
  await fs.writeFile(savedFile.backup, 'tampered');
  await assert.rejects(restore(entry, savedFile), /Backup drift/);
});

test('recovery accepts each original/new wrapper independently and rejects unrelated edits', async t => {
  const root = await scratch(t), entry = join(root, 'wrapper');
  await fs.writeFile(entry, 'original');
  const saved = await snapshot(entry, join(root, 'backup'));
  await assertRecoverableWrapper(entry, saved, digest('new'));
  await fs.writeFile(entry, 'new');
  await assertRecoverableWrapper(entry, saved, digest('new'));
  await fs.writeFile(entry, 'user-edited');
  await assert.rejects(assertRecoverableWrapper(entry, saved, digest('new')), /Entrypoint drift/);
});

test('dynamic CLI follows config.releaseVersion instead of the old symlink version', async t => {
  const root = await scratch(t), core = join(root, 'core'), cli = join(root, 'cli');
  await fs.mkdir(core);
  for (const version of ['6.1.18', '6.1.19']) {
    const runtime = join(core, 'versions', `${version}-${process.platform}-${process.arch}`);
    await fs.mkdir(join(runtime, 'runtime'), { recursive: true });
    await fs.mkdir(join(runtime, 'app'));
    await fs.writeFile(join(runtime, 'runtime/bun'), `#!${process.execPath}\nconsole.log(JSON.stringify({args:process.argv.slice(2),launcher:process.env.CODEX_CHATGPT_WEB_LAUNCHER}));\n`, { mode: 0o755 });
    await fs.writeFile(join(runtime, 'app/cli.js'), '');
    await fs.writeFile(join(runtime, 'manifest.json'), JSON.stringify({ schemaVersion: 2, appVersion: version,
      entrypoint: 'app/cli.js', bundleId: 'a'.repeat(64) }));
  }
  await fs.writeFile(cli, cliWrapper({ 'core-home': core }), { mode: 0o755 });
  const env = { ...process.env }; delete env.CODEX_CHATGPT_WEB_HOME;
  for (const version of ['6.1.18', '6.1.19']) {
    await fs.writeFile(join(core, 'config.json'), JSON.stringify({ releaseVersion: version }));
    const output = JSON.parse(await run(cli, ['doctor', 'a;$(literal)'], { capture: true, env }));
    assert.ok(output.args[0].includes(version));
    assert.deepEqual(output.args.slice(1), ['doctor', 'a;$(literal)']);
    assert.equal(output.launcher, cli);
  }
});

test('isolated prepare preserves dirty source, pins bundled Bun, rejects patch conflict; busy install never stops service', async t => {
  const root = await scratch(t), source = join(root, 'source'), core = join(root, 'core');
  const stateDir = join(root, 'state'), patch = join(root, 'fix.patch');
  const installRoot = join(root, 'lib'), binDir = join(root, 'bin');
  const git = args => run('git', ['-C', source, ...args], { capture: true });
  await fs.mkdir(join(source, 'launcher'), { recursive: true });
  await fs.writeFile(join(source, 'package.json'), JSON.stringify({ version: '6.1.19', packageManager: 'bun@1.4.0' }));
  await fs.writeFile(join(source, 'launcher/package.json'), '{}');
  await fs.writeFile(join(source, 'example.txt'), 'baseline\n');
  await git(['init']); await git(['config', 'user.email', 'test@example.invalid']);
  await git(['config', 'user.name', 'Test']); await git(['add', '.']);
  await git(['commit', '-m', 'baseline']); await git(['tag', 'v6.1.19']);
  await git(['remote', 'add', 'origin', source]);
  await fs.writeFile(join(source, 'example.txt'), 'patched\n');
  await fs.writeFile(patch, await git(['diff', '--binary', '--no-ext-diff', '--no-textconv', 'v6.1.19', '--']));
  const bun = join(core, 'versions', `6.1.19-${process.platform}-${process.arch}`, 'runtime/bun');
  await fs.mkdir(dirname(bun), { recursive: true });
  const bunLog = join(root, 'bun-log');
  await fs.writeFile(bun, `#!${process.execPath}\nconst fs=require('fs');if(process.argv[2]==='--version')console.log('1.4.0');else fs.appendFileSync(${JSON.stringify(bunLog)},JSON.stringify(process.argv.slice(2))+'\\n');\n`, { mode: 0o755 });
  const common = ['--state-dir', stateDir, '--core-home', core, '--install-root', installRoot, '--bin-dir', binDir];
  const before = await git(['status', '--porcelain']);
  await run(process.execPath, [script, 'prepare', '--version', '6.1.19', '--source', source, '--patch', patch, ...common], { capture: true });
  const state = JSON.parse(await fs.readFile(join(stateDir, 'current.json')));
  assert.notEqual(state.source, source);
  assert.equal(await fs.readFile(join(state.source, 'example.txt'), 'utf8'), 'patched\n');
  assert.equal(await git(['status', '--porcelain']), before);
  assert.equal(state.bun.path, bun);
  assert.deepEqual((await fs.readFile(bunLog, 'utf8')).trim().split('\n').map(JSON.parse), [
    ['install', '--frozen-lockfile'], ['install', '--frozen-lockfile']]);
  const badPatch = join(root, 'conflict.patch');
  await fs.writeFile(badPatch, (await fs.readFile(patch, 'utf8')).replace('-baseline', '-not-the-baseline'));
  await assert.rejects(run(process.execPath, [script, 'prepare', '--version', '6.1.19', '--source', source, '--patch', badPatch, ...common], { capture: true }), /patch.*(failed|does not apply)/s);
  assert.equal(await git(['status', '--porcelain']), before);
  assert.deepEqual(JSON.parse(await fs.readFile(join(stateDir, 'current.json'))), state);

  // A single-version local clone has no future tag: prepare must fetch exactly it.
  const remote = join(root, 'remote.git'), producer = join(root, 'producer');
  await run('git', ['clone', '--bare', source, remote], { capture: true });
  await run('git', ['clone', remote, producer], { capture: true });
  const produce = args => run('git', ['-C', producer, ...args], { capture: true });
  await produce(['config', 'user.email', 'test@example.invalid']); await produce(['config', 'user.name', 'Test']);
  await fs.writeFile(join(producer, 'package.json'), JSON.stringify({ version: '6.1.20', packageManager: 'bun@1.4.0' }));
  await produce(['add', '.']); await produce(['commit', '-m', 'future release']);
  await produce(['tag', 'v6.1.20']); await produce(['push', 'origin', 'refs/tags/v6.1.20']);
  await git(['remote', 'set-url', 'origin', remote]);
  await assert.rejects(git(['rev-parse', '--verify', 'refs/tags/v6.1.20']));
  await fs.writeFile(join(core, 'config.json'), JSON.stringify({ releaseVersion: '6.1.19' }));
  await run(process.execPath, [script, 'prepare', '--version', '6.1.20', '--source', source, '--patch', patch, ...common], { capture: true });
  const next = JSON.parse(await fs.readFile(join(stateDir, 'current.json')));
  assert.equal(next.version, '6.1.20');
  assert.equal(next.upstreamCommit, (await produce(['rev-parse', 'v6.1.20'])).trim());
  assert.equal(await git(['status', '--porcelain']), before);
  assert.equal(JSON.parse(await fs.readFile(join(source, 'package.json'))).version, '6.1.19');

  // Fixture state reaches the actual installation gate. No real service/AppImage is invoked.
  await fs.mkdir(installRoot); await fs.mkdir(binDir);
  await fs.writeFile(join(installRoot, 'run-appimage'), '#!/bin/sh\nexit 99\n', { mode: 0o755 });
  await fs.writeFile(join(binDir, 'codex-web-gpt'), 'official wrapper');
  const artifact = join(root, 'fixture.AppImage'); await fs.writeFile(artifact, 'fixture');
  state.verified = { at: 'test' };
  state.build = { artifact, artifactSha256: digest('fixture'), bundleId: 'b'.repeat(64) };
  await fs.writeFile(join(stateDir, 'current.json'), JSON.stringify(state));
  const requests = [];
  const server = createServer((req, res) => {
    requests.push(req.url);
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ status: 'ok', service: 'codex-chatgpt-web', pid: process.pid,
      active_http_turns: 1, active_browser_turns: 0 }));
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  t.after(() => new Promise(done => server.close(done)));
  await fs.writeFile(join(core, 'config.json'), JSON.stringify({ host: '127.0.0.1', port: server.address().port }));
  const audit = join(root, 'systemctl-audit');
  await fs.writeFile(join(binDir, 'systemctl'), `#!${process.execPath}\nrequire('fs').writeFileSync(${JSON.stringify(audit)},'CALLED');process.exit(99);\n`, { mode: 0o755 });
  await assert.rejects(run(process.execPath, [script, 'install', ...common], { capture: true,
    env: { ...process.env, PATH: `${binDir}:${process.env.PATH}` } }), /Busy\/unknown/);
  assert.deepEqual(requests, ['/healthz']);
  assert.equal(await fs.readFile(join(binDir, 'codex-web-gpt'), 'utf8'), 'official wrapper');
  await assert.rejects(fs.stat(audit), { code: 'ENOENT' });
  assert.deepEqual(await fs.readdir(join(installRoot, 'local')), []);
});

test('offline rollback recovers a half-switched install, config version and runtime; starts only the old bundle', async t => {
  const cleanups = [];
  const root = await scratch(t, cleanups), source = join(root, 'source'), core = join(root, 'core');
  const bin = join(root, 'bin'), lib = join(root, 'lib'), backup = join(lib, 'local/receipt');
  await fs.mkdir(join(source, 'launcher/electron'), { recursive: true });
  await fs.mkdir(core); await fs.mkdir(bin); await fs.mkdir(backup, { recursive: true });
  // Small strict runtime validator fixture: identity + real file hashes are checked.
  await fs.writeFile(join(source, 'launcher/electron/runtime-install.cjs'), `
const fs=require('fs'),path=require('path'),crypto=require('crypto');
exports.validateRuntimeBundle=(root,expected)=>{const m=JSON.parse(fs.readFileSync(path.join(root,'manifest.json')));
for(const [key,value]of Object.entries({appVersion:expected.version,platform:expected.platform,arch:expected.arch,...(expected.bundleId?{bundleId:expected.bundleId}:{})}))if(m[key]!==value)throw Error('identity mismatch '+key);
for(const f of m.files)if(crypto.createHash('sha256').update(fs.readFileSync(path.join(root,f.path))).digest('hex')!==f.sha256)throw Error('file hash mismatch');};\n`);
  const portServer = createServer();
  await new Promise(done => portServer.listen(0, '127.0.0.1', done));
  const port = portServer.address().port;
  await new Promise(done => portServer.close(done));
  const pidFile = join(root, 'fixture-daemon.pid'), serviceState = join(root, 'service-state');
  const audit = join(root, 'service-audit');
  const configPath = join(core, 'config.json');
  const runtimeRoot = version => join(core, 'versions', `${version}-${process.platform}-${process.arch}`);
  cleanups.push(() => stopDetachedFixture(pidFile, join(runtimeRoot('6.1.19'), 'app/cli.js')));
  async function runtime(path, version) {
    await fs.mkdir(join(path, 'app'), { recursive: true }); await fs.mkdir(join(path, 'runtime'));
    const cli = `const http=require('http');const version=${JSON.stringify(version)};http.createServer((req,res)=>{res.setHeader('content-type','application/json');res.end(JSON.stringify({status:'ok',service:'codex-chatgpt-web',version,pid:process.pid,accepting_turns:true,active_http_turns:0,active_browser_turns:0}));}).listen(${port},'127.0.0.1');\n`;
    await fs.writeFile(join(path, 'app/cli.js'), cli);
    await fs.writeFile(join(path, 'runtime/bun'), 'fixture');
    const files = [{ path: 'app/cli.js', size: Buffer.byteLength(cli), sha256: digest(cli) },
      { path: 'runtime/bun', size: 7, sha256: digest('fixture') }];
    const manifest = { schemaVersion: 2, appVersion: version, platform: process.platform, arch: process.arch,
      entrypoint: 'app/cli.js', launcher: 'bin/codex-chatgpt-web', files, bundleId: digest(JSON.stringify(files)) };
    await fs.writeFile(join(path, 'manifest.json'), JSON.stringify(manifest));
    return manifest;
  }
  const old = await runtime(join(backup, 'runtime'), '6.1.19');
  const current = await runtime(runtimeRoot('6.1.20'), '6.1.20');
  const launcher = join(bin, 'codex-web-gpt'), cli = join(bin, 'codex-chatgpt-web'), runner = join(lib, 'run-appimage');
  const officialCli = join(root, 'official-cli');
  await fs.writeFile(officialCli, 'official CLI'); await fs.symlink(officialCli, cli);
  await fs.writeFile(launcher, 'official wrapper'); await fs.writeFile(runner, 'runner');
  await fs.writeFile(configPath, JSON.stringify({ host: '127.0.0.1', port, releaseVersion: '6.1.19',
    runtimeCommand: [process.execPath, join(runtimeRoot('6.1.19'), 'app/cli.js')] }));
  const backups = { launcher: await snapshot(launcher, join(backup, 'launcher.wrapper')),
    cli: await snapshot(cli, join(backup, 'cli.wrapper')), config: await snapshot(configPath, join(backup, 'config.json')) };
  // Failure occurs after the first wrapper write and a config migration; CLI remains the original link.
  await fs.writeFile(launcher, 'new wrapper');
  await fs.writeFile(configPath, JSON.stringify({ host: '127.0.0.1', port, releaseVersion: '6.1.20',
    runtimeCommand: [process.execPath, join(runtimeRoot('6.1.20'), 'app/cli.js')] }));
  const receiptPath = join(backup, 'receipt.json'), active = join(lib, 'local/active-receipt.json');
  const receipt = { status: 'failed-after-stop', coreHome: core, source, artifact: join(root, 'custom.AppImage'),
    paths: { launcher, cli, runner, config: configPath }, build: { bundleId: current.bundleId },
    before: { version: '6.1.19', bundleId: old.bundleId, runtimeRoot: runtimeRoot('6.1.19'), process: '2147483647:0' },
    backups, wrapperHashes: { launcher: digest('new wrapper'), cli: digest('new cli wrapper') },
    runtimeBackup: join(backup, 'runtime'), runnerSha256: digest('runner'), previousActive: null };
  await fs.writeFile(receiptPath, JSON.stringify(receipt));
  await fs.writeFile(active, JSON.stringify({ receipt: receiptPath }));
  await fs.writeFile(serviceState, 'inactive');
  await fs.writeFile(join(bin, 'systemctl'), `#!${process.execPath}
const fs=require('fs'),{spawn}=require('child_process');const command=process.argv[3];
fs.appendFileSync(${JSON.stringify(audit)},command+'\\n');
if(command==='is-active'){const state=fs.readFileSync(${JSON.stringify(serviceState)},'utf8');console.log(state);process.exit(state==='active'?0:3);}
if(command!=='start')throw Error('Recovery must never stop/kill an unreachable daemon');
const config=JSON.parse(fs.readFileSync(${JSON.stringify(configPath)}));
const child=spawn(config.runtimeCommand[0],[config.runtimeCommand[1]],{detached:true,stdio:'ignore'});child.unref();
const stat=fs.readFileSync('/proc/'+child.pid+'/stat','utf8');
fs.writeFileSync(${JSON.stringify(pidFile)},JSON.stringify({pid:child.pid,startTime:stat.slice(stat.lastIndexOf(')')+2).split(' ')[19]}));fs.writeFileSync(${JSON.stringify(serviceState)},'active');\n`, { mode: 0o755 });
  const args = [script, 'rollback', '--receipt', receiptPath, '--core-home', core, '--bin-dir', bin,
    '--install-root', lib, '--state-dir', join(root, 'state')];
  const env = { ...process.env, PATH: `${bin}:${process.env.PATH}` };
  // Inactive service alone is insufficient: a receipt PID that is still live must block recovery.
  const stat = await fs.readFile(`/proc/${process.pid}/stat`, 'utf8');
  const identity = `${process.pid}:${stat.slice(stat.lastIndexOf(')') + 2).split(' ')[19]}`;
  receipt.before.process = identity;
  await fs.writeFile(receiptPath, JSON.stringify(receipt));
  await assert.rejects(run(process.execPath, args, { capture: true, env }), /still exists/);
  assert.equal(await fs.readFile(launcher, 'utf8'), 'new wrapper');
  receipt.before.process = '2147483647:0';
  await fs.writeFile(receiptPath, JSON.stringify(receipt));
  const output = await run(process.execPath, args, { capture: true, env });
  assert.match(output, new RegExp(`Rolled back; bundleId=${old.bundleId}`));
  assert.equal(await fs.readFile(launcher, 'utf8'), 'official wrapper');
  assert.equal(await fs.readlink(cli), officialCli);
  assert.equal(JSON.parse(await fs.readFile(configPath)).releaseVersion, '6.1.19');
  const restored = JSON.parse(await fs.readFile(receiptPath));
  assert.equal(restored.status, 'rolled-back');
  assert.equal(restored.restored.bundleId, old.bundleId);
  assert.equal(JSON.parse(await fs.readFile(restored.recoveryConfigBackup)).releaseVersion, '6.1.20');
  assert.equal((await fs.readFile(audit, 'utf8')).trim().split('\n').filter(x => x === 'start').length, 1);
  assert.ok(!(await fs.readFile(audit, 'utf8')).includes('stop'));
  await assert.rejects(fs.stat(active), { code: 'ENOENT' });
});
