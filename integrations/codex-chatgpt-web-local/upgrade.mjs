#!/usr/bin/env node
// Local maintenance only. All subprocesses use argv and shell:false.
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import * as fs from 'node:fs/promises';
import { constants } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve, delimiter } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';

const here = dirname(fileURLToPath(import.meta.url));
const sha = value => createHash('sha256').update(value).digest('hex');
const json = async path => JSON.parse(await fs.readFile(path, 'utf8'));
const exists = async path => fs.lstat(path).then(() => true, error => {
  if (error.code === 'ENOENT') return false;
  throw error;
});
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const pause = ms => new Promise(done => setTimeout(done, ms));
const fileHash = async path => sha(await fs.readFile(path));

export async function run(executable, args, { cwd, env = process.env, capture = false, allowExitCodes = [0] } = {}) {
  return new Promise((done, fail) => {
    const child = spawn(executable, args, { cwd, env, shell: false,
      stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit' });
    let stdout = '', stderr = '';
    if (capture) {
      child.stdout.setEncoding('utf8').on('data', value => { stdout += value; });
      child.stderr.setEncoding('utf8').on('data', value => { stderr += value; });
    }
    child.on('error', fail);
    child.on('close', (code, signal) => !signal && allowExitCodes.includes(code) ? done(stdout) :
      fail(new Error(`${executable} ${args[0] ?? ''} failed (${signal ?? code})${stderr ? `: ${stderr.trim()}` : ''}`)));
  });
}

async function atomic(path, data, mode = 0o600) {
  await fs.mkdir(dirname(path), { recursive: true, mode: 0o700 });
  const temporary = `${path}.tmp-${randomUUID()}`;
  try {
    const handle = await fs.open(temporary, 'wx', mode);
    try { await handle.writeFile(data); await handle.sync(); } finally { await handle.close(); }
    await fs.rename(temporary, path); // replaces a symlink itself, never its target
  } finally { await fs.rm(temporary, { force: true }); }
}
const writeJson = (path, value) => atomic(path, `${JSON.stringify(value, null, 2)}\n`);

export function options(argv) {
  const [command = 'status', ...rest] = argv;
  assert(['status', 'prepare', 'verify', 'build', 'install', 'stage', 'finalize', 'rollback', 'help'].includes(command), `Unknown command: ${command}`);
  const opts = { command };
  const allowed = ['version', 'source', 'patch', 'state-dir', 'core-home', 'install-root', 'bin-dir', 'browser', 'electron', 'receipt'];
  for (let i = 0; i < rest.length; i += 2) {
    const key = rest[i].replace(/^--/, '');
    assert(rest[i].startsWith('--') && allowed.includes(key) && rest[i + 1] && !rest[i + 1].startsWith('--'), `Expected --option value, received ${rest[i]}`);
    assert(opts[key] === undefined, `Duplicate --${key}`);
    opts[key] = rest[i + 1];
  }
  const home = homedir();
  for (const [key, fallback] of Object.entries({
    source: join(home, '.local/src/codex-chatgpt-web'), patch: join(here, 'local-fixes.patch'),
    'state-dir': join(here, '.state'), 'core-home': process.env.CODEX_CHATGPT_WEB_HOME || join(home, '.codex-chatgpt-web'),
    'install-root': join(home, '.local/lib/codex-web-gpt'), 'bin-dir': join(home, '.local/bin'),
  })) opts[key] = resolve(opts[key] || fallback);
  opts.explicitSource = rest.includes('--source');
  return opts;
}

function paths(opts) {
  return { state: join(opts['state-dir'], 'current.json'),
    active: join(opts['install-root'], 'local/active-receipt.json'),
    launcher: join(opts['bin-dir'], 'codex-web-gpt'), cli: join(opts['bin-dir'], 'codex-chatgpt-web'),
    runner: join(opts['install-root'], 'run-appimage'), config: join(opts['core-home'], 'config.json') };
}
async function lock(directory, action) {
  await fs.mkdir(directory, { recursive: true, mode: 0o700 });
  const path = join(directory, '.upgrade.lock');
  let handle;
  try { handle = await fs.open(path, 'wx', 0o600); }
  catch (error) { throw new Error(`Maintenance lock exists: ${path}; check its PID before manually removing it. ${error.message}`); }
  try { await handle.writeFile(`${process.pid}\n`); return await action(); }
  finally { await handle.close(); await fs.unlink(path); }
}
const git = (source, args) => run('git', ['-C', source, ...args], { capture: true });
async function tagCommit(source, version) {
  assert(/^\d+\.\d+\.\d+$/.test(version || ''), 'An explicit --version X.Y.Z is required (never selects latest).');
  return (await git(source, ['rev-parse', '--verify', `refs/tags/v${version}^{commit}`])).trim();
}
async function sourceDigest(source, commit) {
  const untracked = (await git(source, ['ls-files', '--others', '--exclude-standard'])).trim();
  assert(!untracked, `Untracked source files must be reviewed/included in the patch first:\n${untracked}`);
  return sha(await git(source, ['diff', '--binary', '--no-ext-diff', '--no-textconv', commit, '--']));
}
async function assertSource(state) {
  assert((await git(state.source, ['rev-parse', 'HEAD'])).trim() === state.head, 'Source HEAD changed; run verify again.');
  assert(await sourceDigest(state.source, state.upstreamCommit) === state.sourceDigest, 'Source changed; re-export patch and run verify again.');
  assert(await fileHash(state.patchPath) === state.patchSha256, 'Patch changed; run verify again.');
}
function assertSelection(opts, state) {
  if (opts.version) assert(opts.version === state.version, '--version differs from the recorded source; run verify with --source first.');
  if (opts.explicitSource) assert(opts.source === state.source, '--source differs from the verified source; run verify with --source first.');
}
async function bundledBun(opts, state) {
  const pkg = await json(join(state.source, 'package.json'));
  const match = /^bun@(\d+\.\d+\.\d+)$/.exec(pkg.packageManager || '');
  assert(match, 'Upstream packageManager must pin an exact Bun version.');
  const expected = match[1];
  let config = {};
  if (await exists(paths(opts).config)) config = await json(paths(opts).config);
  const candidates = [config.bundledBun,
    join(opts['core-home'], 'versions', `${config.releaseVersion || state.version}-${process.platform}-${process.arch}`, 'runtime/bun'),
    join(opts['core-home'], 'versions', `${state.version}-${process.platform}-${process.arch}`, 'runtime/bun'),
    process.env.CODEX_WEB_GPT_BUN, process.env.CODEX_CHATGPT_WEB_BUN, config.runtimeCommand?.[0], 'bun'];
  for (const candidate of [...new Set(candidates.filter(x => typeof x === 'string' && x))]) {
    try {
      if ((await run(candidate, ['--version'], { capture: true })).trim() === expected) {
        // Resolve PATH candidates so Bun's nested scripts and runtime bundling use the same binary.
        if (candidate.includes('/')) return { path: await fs.realpath(candidate), version: expected };
        for (const directory of (process.env.PATH || '').split(delimiter)) {
          const absolute = join(directory, candidate);
          try { await fs.access(absolute, constants.X_OK); return { path: await fs.realpath(absolute), version: expected }; } catch {}
        }
      }
    } catch {}
  }
  throw new Error(`No matching Bun ${expected}; installed bundled Bun is preferred, global mismatches are refused.`);
}
function buildEnv(opts, bun) {
  const env = { ...process.env, PATH: `${dirname(bun.path)}${delimiter}${process.env.PATH || ''}`,
    CODEX_WEB_GPT_BUN: bun.path, CODEX_CHATGPT_WEB_BUN: bun.path, CODEX_CHATGPT_WEB_EMBEDDED_BUN: bun.path };
  if (opts.browser) env.CHATGPT_DOM_TEST_BROWSER = resolve(opts.browser);
  if (opts.electron) env.LAUNCHER_TEST_ELECTRON = resolve(opts.electron);
  return env;
}
async function frozen(state, bun, env) {
  for (const cwd of [state.source, join(state.source, 'launcher')]) {
    await run(bun.path, ['install', '--frozen-lockfile'], { cwd, env });
  }
}

async function prepareLinuxPackaging(opts, state, env) {
  assert(process.platform === 'linux', 'This maintenance entry packages Linux AppImages.');
  const inputRecord = join(opts['state-dir'], 'linux-inputs/current.json');
  const remembered = state.linuxPackaging || (await exists(inputRecord) ? await json(inputRecord) : undefined);
  let library = env.CODEX_WEB_GPT_LINUX_LIBNOTIFY?.trim() || remembered?.library;
  if (!library) {
    library = join(state.source, 'launcher/build/linux-libs/libnotify.so.4');
    await run('sh', [join(state.source, 'scripts/prepare-linux-libnotify.sh')], {
      cwd: state.source, env: { ...env, GITHUB_ENV: '', CODEX_WEB_GPT_LINUX_LIBNOTIFY_OUTPUT: library },
    });
  }
  assert(library.startsWith('/'), 'CODEX_WEB_GPT_LINUX_LIBNOTIFY must be an absolute library path.');
  const digest = await fileHash(library);
  if (!env.CODEX_WEB_GPT_LINUX_LIBNOTIFY && remembered) {
    assert(digest === remembered.sha256, 'Recorded Linux libnotify input changed.');
  }
  const require = createRequire(import.meta.url);
  const { requireLibnotifySymbol } = require(join(state.source, 'launcher/scripts/prepare-linux-appimage-tools.cjs'));
  requireLibnotifySymbol(library);
  const cached = join(opts['state-dir'], 'linux-inputs', digest, 'libnotify.so.4');
  if (await exists(cached)) assert(await fileHash(cached) === digest, 'Cached libnotify hash drift.');
  else await atomic(cached, await fs.readFile(library), 0o755);
  env.CODEX_WEB_GPT_LINUX_LIBNOTIFY = cached;
  env.CODEX_WEB_GPT_APPIMAGE_TOOLS_OUTPUT = join(state.source, 'launcher/build/appimage-tools');
  await run(state.bun.path, ['run', 'launcher/scripts/prepare-linux-appimage-tools.cjs'], {
    cwd: state.source, env: { ...env, GITHUB_ENV: '', APPIMAGE_TOOLS_PATH: '' },
  });
  env.APPIMAGE_TOOLS_PATH = env.CODEX_WEB_GPT_APPIMAGE_TOOLS_OUTPUT;
  state.linuxPackaging = { library: cached, sha256: digest, toolPreparation: 'upstream prepare-linux-appimage-tools.cjs' };
  await writeJson(inputRecord, state.linuxPackaging);
  await writeJson(paths(opts).state, state);
}

async function bindSource(opts) {
  const pkg = await json(join(opts.source, 'package.json'));
  const version = opts.version || pkg.version;
  assert(pkg.version === version, 'Source package version differs from --version.');
  const upstreamCommit = await tagCommit(opts.source, version);
  const patchSha256 = await fileHash(opts.patch);
  assert((await fs.stat(opts.patch)).size > 0, 'Patch is empty.');
  const digest = await sourceDigest(opts.source, upstreamCommit);
  assert(digest === patchSha256,
    `Source diff does not exactly match ${opts.patch}. Export with git diff --binary --no-ext-diff --no-textconv v${version} -- (include new files with git add -N first).`);
  return { schemaVersion: 1, version, upstreamCommit, source: opts.source,
    head: (await git(opts.source, ['rev-parse', 'HEAD'])).trim(), patchPath: opts.patch,
    patchSha256, sourceDigest: digest, createdAt: new Date().toISOString() };
}
async function prepare(opts) {
  assert(/^\d+\.\d+\.\d+$/.test(opts.version || ''), 'prepare requires explicit --version X.Y.Z.');
  assert(await exists(opts.source), `Source clone is missing: ${opts.source}. Clone upstream (or use fetchrepo) and pass --source PATH first.`);
  // Fetch only the requested tag. No forced tag update, checkout, reset or dirty-file mutation.
  await git(opts.source, ['fetch', '--no-tags', 'origin', `refs/tags/v${opts.version}:refs/tags/v${opts.version}`]);
  const commit = await tagCommit(opts.source, opts.version);
  const digest = await fileHash(opts.patch);
  assert((await fs.stat(opts.patch)).size > 0, 'Patch is empty.');
  await fs.mkdir(join(opts['state-dir'], 'sources'), { recursive: true });
  const source = await fs.mkdtemp(join(opts['state-dir'], 'sources', `${opts.version}-${digest.slice(0, 12)}-`));
  console.log(`Preparing isolated clone: ${source} (failed clones are retained for review)`);
  await run('git', ['clone', '--no-hardlinks', '--no-checkout', '--', opts.source, source]);
  await git(source, ['checkout', '--detach', commit]);
  await git(source, ['apply', '--check', opts.patch]);
  await git(source, ['apply', '--index', opts.patch]);
  const state = await bindSource({ ...opts, source });
  state.bun = await bundledBun(opts, state);
  await frozen(state, state.bun, buildEnv(opts, state.bun));
  await assertSource(state);
  await writeJson(paths(opts).state, state);
  console.log(`Prepared ${state.version} at ${source}; run verify, then build.`);
}

async function configureElectron(state, env) {
  assert(env.CHATGPT_DOM_TEST_BROWSER && env.LAUNCHER_TEST_ELECTRON,
    'verify requires --browser /path/to/chromium and --electron /path/to/electron (or CHATGPT_DOM_TEST_BROWSER / LAUNCHER_TEST_ELECTRON). Browser regressions must not silently skip.');
  await fs.access(env.CHATGPT_DOM_TEST_BROWSER, constants.X_OK);
  await fs.access(env.LAUNCHER_TEST_ELECTRON, constants.X_OK);
  const version = (await run(env.LAUNCHER_TEST_ELECTRON, ['-p', 'process.versions.electron'], {
    capture: true, env: { ...env, ELECTRON_RUN_AS_NODE: '1' },
  })).trim();
  const electronRoot = join(state.source, 'launcher/node_modules/electron');
  assert(version === (await json(join(electronRoot, 'package.json'))).version,
    `Installed Electron ${version} does not match upstream launcher Electron.`);
  const dist = join(electronRoot, 'dist');
  if (await exists(dist)) {
    assert(!(await fs.lstat(dist)).isSymbolicLink(), `Refusing to alter symlink ${dist}`);
    // Preserve an already installed matching distribution.
    const currentVersion = await fs.readFile(join(dist, 'version'), 'utf8').catch(() => '');
    if (currentVersion.trim().replace(/^v/, '') === version && await exists(join(dist, 'electron'))) return;
    throw new Error(`Incomplete/different Electron dist at ${dist}; review it before retrying.`);
  }
  // A private copy prevents upstream install.js from writing into the user's installed Electron.
  await fs.cp(dirname(await fs.realpath(env.LAUNCHER_TEST_ELECTRON)), dist, { recursive: true });
  const executableName = (await fs.realpath(env.LAUNCHER_TEST_ELECTRON)).split('/').at(-1);
  if (executableName !== 'electron') await fs.copyFile(join(dist, executableName), join(dist, 'electron'));
  await fs.chmod(join(dist, 'electron'), 0o755);
  await fs.writeFile(join(dist, 'version'), version);
  await fs.writeFile(join(electronRoot, 'path.txt'), 'electron');
}
async function verify(opts) {
  let state = !opts.explicitSource && await exists(paths(opts).state) ? await json(paths(opts).state) : await bindSource(opts);
  assertSelection(opts, state);
  await assertSource(state);
  // Invalidate old verification/build evidence before any potentially failing check.
  delete state.verified; delete state.build;
  await writeJson(paths(opts).state, state);
  state.bun = await bundledBun(opts, state);
  const env = buildEnv(opts, state.bun);
  await frozen(state, state.bun, env);
  await configureElectron(state, env);
  await run(state.bun.path, ['run', 'verify'], { cwd: state.source, env });
  await assertSource(state);
  state.verified = { at: new Date().toISOString(), browser: env.CHATGPT_DOM_TEST_BROWSER, electron: env.LAUNCHER_TEST_ELECTRON };
  await writeJson(paths(opts).state, state);
  console.log(`Verified ${state.version}; patch ${state.patchSha256}`);
}
async function validateRuntime(source, root, version, bundleId) {
  const require = createRequire(import.meta.url);
  const { validateRuntimeBundle } = require(join(source, 'launcher/electron/runtime-install.cjs'));
  validateRuntimeBundle(root, { version, bundleId, platform: process.platform, arch: process.arch });
  return json(join(root, 'manifest.json'));
}
async function inspectArtifact(state, artifact, directory) {
  await fs.mkdir(directory, { recursive: true, mode: 0o700 });
  await run(artifact, ['--appimage-extract'], { cwd: directory, capture: true });
  return validateRuntime(state.source, join(directory, 'squashfs-root/resources/runtime'), state.version);
}
async function build(opts) {
  const state = await json(paths(opts).state);
  assertSelection(opts, state);
  await assertSource(state);
  assert(state.verified, 'Run verify successfully before build.');
  delete state.build;
  await writeJson(paths(opts).state, state);
  state.bun = await bundledBun(opts, state);
  const env = buildEnv(opts, state.bun);
  await frozen(state, state.bun, env);
  await prepareLinuxPackaging(opts, state, env);
  await run(state.bun.path, ['run', 'app:package'], { cwd: state.source, env });
  const artifact = join(state.source, 'launcher/artifacts', `codex-web-gpt-${state.version}-linux-${process.arch}.AppImage`);
  await fs.chmod(artifact, 0o755);
  const scratch = await fs.mkdtemp(join(opts['state-dir'], 'inspect-'));
  let manifest;
  try { manifest = await inspectArtifact(state, artifact, scratch); }
  finally { await fs.rm(scratch, { recursive: true, force: true }); }
  const builtManifest = await validateRuntime(state.source, join(state.source, 'launcher/build/runtime'), state.version, manifest.bundleId);
  assert(builtManifest.bunVersion === state.bun.version, 'Runtime Bun version mismatch.');
  // Upstream package smoke uses its own temporary core-home and launcher data directory.
  await run(state.bun.path, ['run', 'app:smoke'], { cwd: state.source, env });
  await assertSource(state);
  state.build = { at: new Date().toISOString(), artifact, artifactSha256: await fileHash(artifact),
    bundleId: manifest.bundleId, manifest, linuxPackaging: state.linuxPackaging };
  await writeJson(paths(opts).state, state);
  console.log(`Built ${artifact}\nbundleId=${manifest.bundleId}`);
}

export function assertIdle(health) {
  assert(health.status === 'ok' && health.service === 'codex-chatgpt-web' && Number.isSafeInteger(health.pid) && health.pid > 0,
    'Missing trustworthy health evidence.');
  assert(health.active_http_turns === 0 && health.active_browser_turns === 0,
    `Busy/unknown activity: HTTP=${health.active_http_turns}, browser=${health.active_browser_turns}. No service stop or task cancellation allowed.`);
}
async function request(opts, route = '/healthz', control = false) {
  const config = await json(paths(opts).config);
  assert(['127.0.0.1', 'localhost', '::1'].includes(config.host), 'Only a loopback health endpoint is supported.');
  assert(Number.isSafeInteger(config.port) && config.port > 0 && config.port <= 65535, 'Invalid configured health port.');
  if (control) assert(typeof config.controlToken === 'string' && config.controlToken, 'Missing controlToken; cannot drain safely.');
  const host = config.host === '::1' ? '[::1]' : config.host;
  const response = await fetch(`http://${host}:${config.port}${route}`, {
    method: control ? 'POST' : 'GET', signal: AbortSignal.timeout(5000), redirect: 'error',
    headers: control ? { authorization: `Bearer ${config.controlToken}` } : {},
  });
  assert(response.ok, `${route} returned HTTP ${response.status}`);
  return response.json();
}
async function processIdentity(pid) {
  const stat = await fs.readFile(`/proc/${pid}/stat`, 'utf8');
  return `${pid}:${stat.slice(stat.lastIndexOf(')') + 2).split(' ')[19]}`;
}
async function runningIdentity(opts, source, health = undefined) {
  health ??= await request(opts);
  assert(health.status === 'ok' && health.service === 'codex-chatgpt-web', 'Unhealthy runtime.');
  const config = await json(paths(opts).config);
  assert(health.version === config.releaseVersion, 'Health/config version drift.');
  const runtimeRoot = join(opts['core-home'], 'versions', `${config.releaseVersion}-${process.platform}-${process.arch}`);
  const manifest = await validateRuntime(source, runtimeRoot, config.releaseVersion);
  const argv = (await fs.readFile(`/proc/${health.pid}/cmdline`, 'utf8')).split('\0');
  const entrypoint = join(runtimeRoot, manifest.entrypoint);
  assert(argv.includes(entrypoint) && config.runtimeCommand?.includes(entrypoint), 'Health PID/config does not run the expected durable runtime entrypoint.');
  return { version: health.version, bundleId: manifest.bundleId, runtimeRoot,
    process: await processIdentity(health.pid), manifest };
}
async function drain(opts) {
  const health = await request(opts);
  assertIdle(health);
  assert(health.accepting_turns === true, 'Runtime is already drained by another operator.');
  const process = await processIdentity(health.pid);
  let acquired = false;
  try {
    const drained = await request(opts, '/admin/drain', true);
    acquired = true;
    assert(drained.accepting_turns === false && drained.active_http_turns === 0 && drained.active_browser_turns === 0,
      'Work arrived during drain; refusing to stop.');
    const final = await request(opts);
    assertIdle(final);
    assert(final.accepting_turns === false && await processIdentity(final.pid) === process, 'Runtime changed while draining.');
    return { health: final, process };
  } catch (error) {
    if (acquired) await resume(opts, process);
    throw error;
  }
}
async function resume(opts, expectedProcess) {
  try {
    const health = await request(opts);
    if (await processIdentity(health.pid) === expectedProcess) await request(opts, '/admin/resume', true);
  } catch (error) { console.error(`Could not resume original daemon: ${error.message}`); }
}
async function stopIdle(opts, drained) {
  const health = await request(opts);
  assertIdle(health);
  assert(health.accepting_turns === false && await processIdentity(health.pid) === drained.process, 'Drain lost before stop.');
  await run('systemctl', ['--user', 'stop', 'codex-web-gpt']);
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    const alive = await processIdentity(health.pid).catch(() => null);
    if (alive !== drained.process) return;
    await pause(250);
  }
  throw new Error('Service stop left the daemon alive. No runtime replacement or kill attempted; inspect service ownership.');
}
async function startAndCheck(opts, source, expected, previousProcess) {
  await run('systemctl', ['--user', 'start', 'codex-web-gpt']);
  const deadline = Date.now() + 60000;
  let failure;
  while (Date.now() < deadline) {
    try {
      const health = await request(opts);
      assert(health.accepting_turns === true, 'New runtime is not accepting turns.');
      const identity = await runningIdentity(opts, source, health);
      assert(identity.version === expected.version && identity.bundleId === expected.bundleId, 'Running runtime bundleId differs from receipt.');
      assert(identity.process !== previousProcess, 'Old daemon survived service switch.');
      return identity;
    } catch (error) { failure = error; await pause(1000); }
  }
  throw new Error(`Post-start identity/health check failed: ${failure?.message}`);
}

export async function snapshot(path, backup) {
  if (!await exists(path)) return { kind: 'absent' };
  const stat = await fs.lstat(path);
  if (stat.isSymbolicLink()) return { kind: 'symlink', target: await fs.readlink(path) };
  assert(stat.isFile(), `Expected file or symlink: ${path}`);
  await fs.copyFile(path, backup, constants.COPYFILE_EXCL);
  return { kind: 'file', backup, mode: stat.mode & 0o777, sha256: await fileHash(backup) };
}
export async function restore(path, saved) {
  if (saved.kind === 'absent') { await fs.rm(path, { force: true }); return; }
  if (saved.kind === 'file') {
    assert(await fileHash(saved.backup) === saved.sha256, `Backup drift: ${saved.backup}`);
    await atomic(path, await fs.readFile(saved.backup), saved.mode);
    return;
  }
  assert(saved.kind === 'symlink', 'Unknown backup kind.');
  const temporary = `${path}.tmp-${randomUUID()}`;
  try { await fs.symlink(saved.target, temporary); await fs.rename(temporary, path); }
  finally { await fs.rm(temporary, { force: true }); }
}
async function assertSnapshot(path, saved) {
  if (saved.kind === 'absent') { assert(!await exists(path), `Entrypoint appeared: ${path}`); return; }
  assert(await exists(path), `Entrypoint disappeared: ${path}`);
  const stat = await fs.lstat(path);
  if (saved.kind === 'symlink') {
    assert(stat.isSymbolicLink() && await fs.readlink(path) === saved.target, `Symlink drift: ${path}`);
  } else assert(stat.isFile() && !stat.isSymbolicLink() && await fileHash(path) === saved.sha256
    && (stat.mode & 0o777) === saved.mode, `Entrypoint drift: ${path}`);
}
function launcherWrapper(opts, artifact) {
  return `#!${process.execPath}\n'use strict';\nconst {spawn}=require('node:child_process');\nconst args=${JSON.stringify([paths(opts).runner, artifact])};\nconst child=spawn(args[0],[args[1],...process.argv.slice(2)],{stdio:'inherit',shell:false,env:{...process.env,CODEX_WEB_GPT_LAUNCHER_EXECUTABLE:${JSON.stringify(paths(opts).launcher)},CODEX_WEB_GPT_APPIMAGE:args[1]}});\nchild.on('error',e=>{console.error(e.message);process.exit(1)});\nfor(const s of ['SIGTERM','SIGINT','SIGHUP'])process.on(s,()=>child.kill(s));\nchild.on('exit',(code,signal)=>{if(signal){process.removeAllListeners(signal);process.kill(process.pid,signal)}else process.exit(code??1)});\n`;
}
export function cliWrapper(opts) {
  // Read releaseVersion on EVERY invocation, including after an official update.
  return `#!${process.execPath}\n'use strict';\nconst fs=require('node:fs'),path=require('node:path'),{spawn}=require('node:child_process');\ntry{\nconst home=process.env.CODEX_CHATGPT_WEB_HOME||${JSON.stringify(opts['core-home'])};\nconst config=JSON.parse(fs.readFileSync(path.join(home,'config.json'),'utf8'));\nif(!/^\\d+\\.\\d+\\.\\d+$/.test(config.releaseVersion))throw Error('Invalid releaseVersion');\nconst root=path.join(home,'versions',config.releaseVersion+'-'+process.platform+'-'+process.arch);\nconst manifest=JSON.parse(fs.readFileSync(path.join(root,'manifest.json'),'utf8'));\nif(manifest.schemaVersion!==2||manifest.appVersion!==config.releaseVersion||manifest.entrypoint!=='app/cli.js'||!/^[a-f0-9]{64}$/.test(manifest.bundleId))throw Error('Invalid runtime manifest');\nconst child=spawn(path.join(root,'runtime/bun'),[path.join(root,'app/cli.js'),...process.argv.slice(2)],{stdio:'inherit',shell:false,env:{...process.env,CODEX_CHATGPT_WEB_LAUNCHER:__filename}});\nchild.on('error',e=>{console.error(e.message);process.exit(1)});\nfor(const s of ['SIGTERM','SIGINT','SIGHUP'])process.on(s,()=>child.kill(s));\nchild.on('exit',(code,signal)=>{if(signal){process.removeAllListeners(signal);process.kill(process.pid,signal)}else process.exit(code??1)});\n}catch(e){console.error(e.message);process.exit(1)}\n`;
}
async function assertWrappers(receipt) {
  for (const [key, expected] of Object.entries(receipt.wrapperHashes)) {
    const path = receipt.paths[key];
    assert(await exists(path) && !(await fs.lstat(path)).isSymbolicLink() && await fileHash(path) === expected,
      `Wrapper drift: ${path}. Review it before rollback/install; it will not be overwritten.`);
  }
}
async function install(opts, stageOnly = false) {
  const state = await json(paths(opts).state);
  assertSelection(opts, state);
  await assertSource(state);
  assert(state.build && state.verified, 'A successful verify and build are required.');
  assert(await fileHash(state.build.artifact) === state.build.artifactSha256, 'Artifact hash drift.');
  await fs.access(paths(opts).runner, constants.X_OK);
  await fs.access(opts['bin-dir'], constants.W_OK);
  const health = await request(opts);
  // Staging changes only future launch entrypoints. The current service and runtime stay live.
  if (!stageOnly) assertIdle(health);
  const before = await runningIdentity(opts, state.source, health);
  if (await exists(paths(opts).active)) {
    const active = await json((await json(paths(opts).active)).receipt);
    assert(active.status === 'installed', 'An unfinished installation exists; inspect/rollback its receipt first.');
    await assertWrappers(active);
    assert(before.bundleId === active.build.bundleId, 'Active receipt/runtime bundleId drift.');
  }
  const destination = join(opts['install-root'], 'local', `${state.version}-${state.patchSha256}`);
  await fs.mkdir(destination, { recursive: true, mode: 0o700 });
  const artifact = join(destination, 'Codex Web GPT.AppImage');
  if (await exists(artifact)) assert(await fileHash(artifact) === state.build.artifactSha256, 'Immutable install destination already contains a different artifact.');
  else await atomic(artifact, await fs.readFile(state.build.artifact), 0o755);
  const backup = join(destination, 'receipts', `${Date.now()}-${randomUUID()}`);
  await fs.mkdir(backup, { recursive: true, mode: 0o700 });
  const receiptPath = join(backup, 'receipt.json');
  const p = paths(opts);
  const receipt = { schemaVersion: 1, status: 'backing-up', createdAt: new Date().toISOString(),
    version: state.version, upstreamCommit: state.upstreamCommit, patchSha256: state.patchSha256,
    source: state.source, coreHome: opts['core-home'], paths: p, artifact,
    build: state.build, before, previousActive: await exists(p.active) ? await json(p.active) : null,
    backups: { launcher: await snapshot(p.launcher, join(backup, 'launcher.wrapper')),
      cli: await snapshot(p.cli, join(backup, 'cli.wrapper')),
      config: await snapshot(p.config, join(backup, 'config.json')) },
    runtimeBackup: join(backup, 'runtime'), wrapperHashes: {},
    runnerSha256: await fileHash(p.runner) };
  await writeJson(receiptPath, receipt);
  // Copy before drain to keep the interruption short, validate and recheck identity after drain.
  await fs.cp(before.runtimeRoot, receipt.runtimeBackup, { recursive: true, verbatimSymlinks: true });
  await validateRuntime(state.source, receipt.runtimeBackup, before.version, before.bundleId);
  const launcher = launcherWrapper(opts, artifact), cli = cliWrapper(opts);
  receipt.wrapperHashes = { launcher: sha(launcher), cli: sha(cli) };
  receipt.status = 'ready';
  await writeJson(receiptPath, receipt);
  console.log(`Receipt and rollback backup: ${receiptPath}`);
  if (stageOnly) {
    const current = await runningIdentity(opts, state.source);
    assert(current.process === before.process && current.bundleId === before.bundleId, 'Runtime changed during staging.');
    for (const key of ['launcher', 'cli', 'config']) await assertSnapshot(p[key], receipt.backups[key]);
    receipt.status = 'staging';
    await writeJson(receiptPath, receipt);
    try {
      // Single-instance locking in upstream main.cjs precedes runtime installation.
      // A second launch while the old GUI is open only activates that old GUI.
      await atomic(p.launcher, launcher, 0o755);
      await atomic(p.cli, cli, 0o755);
      await writeJson(p.active, { receipt: receiptPath });
      receipt.status = 'staged';
      receipt.stagedAt = new Date().toISOString();
      await writeJson(receiptPath, receipt);
      console.log(`Staged ${artifact}\nCurrent runtime was not stopped or replaced. Fully quit and reopen the Launcher, then run finalize.\nReceipt: ${receiptPath}`);
      return;
    } catch (error) {
      // Undo only this entrypoint transaction; never stop or restore a live runtime.
      for (const key of ['launcher', 'cli']) {
        await assertRecoverableWrapper(p[key], receipt.backups[key], receipt.wrapperHashes[key]);
        await restore(p[key], receipt.backups[key]);
      }
      if (receipt.previousActive) await writeJson(p.active, receipt.previousActive);
      else await fs.rm(p.active, { force: true });
      receipt.status = 'staging-failed';
      await writeJson(receiptPath, receipt);
      throw error;
    }
  }
  const drained = await drain(opts);
  let stopped = false;
  try {
    const current = await runningIdentity(opts, state.source, drained.health);
    assert(current.process === before.process && current.bundleId === before.bundleId, 'Runtime changed before service stop.');
    // Backups must still describe the entrypoints at the actual switching boundary.
    for (const key of ['launcher', 'cli', 'config']) await assertSnapshot(p[key], receipt.backups[key]);
    assert(await fileHash(p.runner) === receipt.runnerSha256, 'AppImage runner changed during backup.');
    await stopIdle(opts, drained);
    stopped = true;
    receipt.status = 'switching';
    await writeJson(receiptPath, receipt);
    await writeJson(p.active, { receipt: receiptPath });
    await atomic(p.launcher, launcher, 0o755);
    await atomic(p.cli, cli, 0o755);
    receipt.status = 'starting';
    await writeJson(receiptPath, receipt);
    receipt.running = await startAndCheck(opts, state.source, { version: state.version, bundleId: state.build.bundleId }, before.process);
    await assertWrappers(receipt);
    receipt.installedConfigSha256 = await fileHash(p.config);
    receipt.status = 'installed';
    await writeJson(receiptPath, receipt);
    console.log(`Installed ${artifact}\nbundleId=${receipt.running.bundleId}\nRollback: node ${join(here, 'upgrade.mjs')} rollback --receipt ${receiptPath}`);
  } catch (error) {
    receipt.status = stopped ? 'failed-after-stop' : 'failed-before-stop';
    receipt.error = error.message;
    receipt.failureConfigSha256 = await fileHash(p.config).catch(() => null);
    await writeJson(receiptPath, receipt);
    if (!stopped) await resume(opts, drained.process);
    throw new Error(`${error.message}\nReceipt: ${receiptPath}; no automatic kill or restart retry. Inspect health and use rollback when idle.`);
  }
}
async function stage(opts) { return install(opts, true); }
async function finalize(opts) {
  const receiptPath = (await json(paths(opts).active)).receipt;
  const receipt = await json(receiptPath);
  assert(receipt.status === 'staged', 'Only a staged installation can be finalized.');
  await assertWrappers(receipt);
  assert(await fileHash(receipt.artifact) === receipt.build.artifactSha256, 'Staged artifact hash drift.');
  const current = await runningIdentity(opts, receipt.source);
  assert(current.bundleId === receipt.build.bundleId && current.process !== receipt.before.process,
    'The staged runtime has not started. Fully quit and reopen the Launcher first; no stop/restart attempted.');
  receipt.running = current;
  receipt.installedConfigSha256 = await fileHash(paths(opts).config);
  receipt.status = 'installed';
  receipt.finalizedAt = new Date().toISOString();
  await writeJson(receiptPath, receipt);
  console.log(`Finalized ${receipt.artifact}\nbundleId=${current.bundleId}`);
}
export async function assertRecoverableWrapper(path, saved, expectedHash) {
  if (await exists(path)) {
    const stat = await fs.lstat(path);
    if (stat.isFile() && !stat.isSymbolicLink() && await fileHash(path) === expectedHash) return;
  }
  await assertSnapshot(path, saved);
}
export async function confirmOffline(opts, receipt) {
  const active = (await run('systemctl', ['--user', 'is-active', 'codex-web-gpt'], {
    capture: true, allowExitCodes: [0, 3],
  })).trim();
  assert(['inactive', 'failed'].includes(active), `Recovery requires service inactive/failed; observed ${active || 'unknown'}. No stop/kill attempted.`);
  let health;
  try { health = await request(opts); }
  catch (error) {
    // A timeout, malformed response, or authentication error is not absence evidence.
    assert(error.cause?.code === 'ECONNREFUSED', `Cannot prove health endpoint is offline: ${error.message}`);
  }
  assert(!health, 'A runtime still answers health; use the idle drain path, never offline recovery.');
  for (const identity of [receipt.before.process, receipt.running?.process].filter(Boolean)) {
    const pid = Number(identity.split(':')[0]);
    assert(await processIdentity(pid).catch(() => null) !== identity, `Receipt daemon ${identity} still exists; no stop/kill attempted.`);
  }
  // Catch a newly spawned daemon even if it never reached healthz. Inspect only
  // this user's processes; inability to inspect one is a refusal, not an idle claim.
  const core = opts['core-home'];
  for (const name of await fs.readdir('/proc')) {
    if (!/^\d+$/.test(name)) continue;
    let argv;
    try {
      if ((await fs.stat(`/proc/${name}`)).uid !== process.getuid()) continue;
      argv = (await fs.readFile(`/proc/${name}/cmdline`, 'utf8')).split('\0').filter(Boolean);
    } catch (error) {
      if (['ENOENT', 'ESRCH'].includes(error.code)) continue;
      throw error;
    }
    let launcherForCore = false;
    if (argv.some(arg => /(?:^|\/)codex-web-gpt-launcher$/.test(arg))) {
      try {
        const environment = (await fs.readFile(`/proc/${name}/environ`, 'utf8')).split('\0');
        const configured = environment.find(value => value.startsWith('CODEX_CHATGPT_WEB_HOME='));
        launcherForCore = (configured ? configured.slice('CODEX_CHATGPT_WEB_HOME='.length) : join(homedir(), '.codex-chatgpt-web')) === core;
      } catch (error) { if (!['ENOENT', 'ESRCH'].includes(error.code)) throw error; }
    }
    assert(!launcherForCore && !argv.some(arg => (arg.startsWith(`${core}/versions/`) && arg.endsWith('/app/cli.js'))
      || arg === receipt.artifact || arg === receipt.paths.launcher || arg === receipt.paths.runner),
    `Related runtime/launcher PID ${name} is still live; no stop/kill attempted.`);
  }
}
async function rollback(opts) {
  const p = paths(opts);
  const receiptPath = resolve(opts.receipt || (await json(p.active)).receipt);
  const receipt = await json(receiptPath);
  assert(receipt.coreHome === opts['core-home'] && receipt.paths.launcher === p.launcher && receipt.paths.cli === p.cli,
    'Receipt paths differ from supplied core-home/bin-dir.');
  assert(!['rolled-back', 'unstaged'].includes(receipt.status), 'Receipt already rolled back or staging cancelled.');
  if (await exists(p.active)) assert(resolve((await json(p.active)).receipt) === receiptPath,
    'Receipt is not the active installation; roll back the active receipt first.');
  assert(await fileHash(p.runner) === receipt.runnerSha256, 'AppImage runner drift; review before rollback.');
  if (['staging', 'staged'].includes(receipt.status)) {
    const current = await runningIdentity(opts, receipt.source);
    assert(current.bundleId === receipt.before.bundleId,
      'Staged package already activated or runtime drifted; finalize it before normal rollback.');
    for (const key of ['launcher', 'cli']) {
      await assertRecoverableWrapper(p[key], receipt.backups[key], receipt.wrapperHashes[key]);
      const saved = receipt.backups[key];
      if (saved.kind === 'file') assert(await fileHash(saved.backup) === saved.sha256, `Backup drift: ${saved.backup}`);
    }
    for (const key of ['launcher', 'cli']) await restore(p[key], receipt.backups[key]);
    receipt.status = 'unstaged';
    await writeJson(receiptPath, receipt);
    if (receipt.previousActive) await writeJson(p.active, receipt.previousActive);
    else await fs.rm(p.active, { force: true });
    console.log('Staging cancelled; current runtime was not stopped or replaced.');
    return;
  }
  const recovering = ['failed-after-stop', 'starting', 'switching', 'rolling-back', 'recovery-failed'].includes(receipt.status);
  if (recovering) {
    for (const key of ['launcher', 'cli']) await assertRecoverableWrapper(p[key], receipt.backups[key], receipt.wrapperHashes[key]);
  } else {
    await assertWrappers(receipt);
    assert(await fileHash(p.config) === receipt.installedConfigSha256,
      'Config changed since installation; review/merge it with the receipt config backup before rollback.');
  }
  let health;
  try { health = await request(opts); }
  catch (error) { if (!recovering) throw error; }
  let current, drained;
  if (health) {
    assertIdle(health);
    current = await runningIdentity(opts, receipt.source, health);
    assert(current.bundleId === receipt.build.bundleId || (recovering && current.bundleId === receipt.before.bundleId),
      'Runtime drift: health bundleId matches neither the installed bundle nor recovery backup.');
  } else await confirmOffline(opts, receipt);
  // Validate all backups before stopping anything or restoring the first entrypoint.
  await validateRuntime(receipt.source, receipt.runtimeBackup, receipt.before.version, receipt.before.bundleId);
  for (const saved of Object.values(receipt.backups)) {
    if (saved.kind === 'file') assert(await fileHash(saved.backup) === saved.sha256, `Backup drift: ${saved.backup}`);
  }
  if (health) drained = await drain(opts);
  let stopped = !health;
  try {
    if (health) {
      const boundary = await runningIdentity(opts, receipt.source, drained.health);
      assert(boundary.process === current.process && boundary.bundleId === current.bundleId, 'Runtime changed before rollback stop.');
      await stopIdle(opts, drained);
      stopped = true;
    }
    await confirmOffline(opts, receipt);
    if (recovering) {
      for (const key of ['launcher', 'cli']) await assertRecoverableWrapper(p[key], receipt.backups[key], receipt.wrapperHashes[key]);
      // An interrupted upgrade may have migrated config before becoming healthy.
      // Retain that private file for review rather than losing later edits.
      if (await exists(p.config)) {
        const saved = join(dirname(receiptPath), `config-before-recovery-${Date.now()}.json`);
        await fs.copyFile(p.config, saved, constants.COPYFILE_EXCL);
        await fs.chmod(saved, 0o600);
        receipt.recoveryConfigBackup = saved;
      }
    } else {
      await assertWrappers(receipt);
      assert(await fileHash(p.config) === receipt.installedConfigSha256, 'Config changed at rollback boundary.');
    }
    receipt.status = 'rolling-back';
    await writeJson(receiptPath, receipt);
    // Never overwrite the live runtime. Service/daemon are now confirmed stopped.
    const destination = receipt.before.runtimeRoot;
    const temporary = `${destination}.restore-${randomUUID()}`;
    const displaced = `${destination}.displaced-${randomUUID()}`;
    await fs.cp(receipt.runtimeBackup, temporary, { recursive: true, verbatimSymlinks: true });
    await validateRuntime(receipt.source, temporary, receipt.before.version, receipt.before.bundleId);
    const hadDestination = await exists(destination);
    if (hadDestination) await fs.rename(destination, displaced);
    try { await fs.rename(temporary, destination); }
    catch (error) { if (hadDestination) await fs.rename(displaced, destination); throw error; }
    if (hadDestination) receipt.displacedRuntime = displaced; // retain for review/recovery
    await restore(p.launcher, receipt.backups.launcher);
    await restore(p.cli, receipt.backups.cli);
    // An older launcher refuses a config with a newer releaseVersion. Restore its
    // complete private config as well; the hash gate above prevents lost edits.
    await restore(p.config, receipt.backups.config);
    receipt.restored = await startAndCheck(opts, receipt.source, receipt.before, current?.process || receipt.before.process);
    receipt.status = 'rolled-back';
    receipt.rolledBackAt = new Date().toISOString();
    await writeJson(receiptPath, receipt);
    if (receipt.previousActive) await writeJson(p.active, receipt.previousActive);
    else await fs.rm(p.active, { force: true });
    console.log(`Rolled back; bundleId=${receipt.restored.bundleId}`);
  } catch (error) {
    receipt.error = error.message;
    if (stopped) receipt.status = 'recovery-failed';
    await writeJson(receiptPath, receipt);
    if (!stopped && drained) await resume(opts, drained.process);
    throw error;
  }
}
async function status(opts) {
  const p = paths(opts), result = { patch: opts.patch, patchSha256: null, state: p.state, activeReceipt: null };
  if (await exists(opts.patch)) result.patchSha256 = await fileHash(opts.patch);
  let state;
  if (await exists(p.state)) {
    state = await json(p.state);
    result.prepared = { version: state.version, upstreamCommit: state.upstreamCommit, source: state.source,
      patchSha256: state.patchSha256, verified: state.verified, artifact: state.build?.artifact, bundleId: state.build?.bundleId };
    try { await assertSource(state); result.sourceMatches = true; } catch (error) { result.sourceError = error.message; }
  }
  if (await exists(p.active)) {
    result.activeReceipt = (await json(p.active)).receipt;
    const receipt = await json(result.activeReceipt);
    result.installation = { status: receipt.status, artifact: receipt.artifact, bundleId: receipt.build.bundleId };
    try {
      await assertWrappers(receipt);
      assert(await fileHash(receipt.artifact) === receipt.build.artifactSha256, 'Installed artifact hash drift.');
      const actual = await runningIdentity(opts, receipt.source);
      const awaitingRestart = receipt.status === 'staged' && actual.bundleId === receipt.before.bundleId;
      assert(actual.bundleId === receipt.build.bundleId || awaitingRestart, 'Running bundleId differs from receipt (official update may have replaced the patch).');
      result.running = { version: actual.version, bundleId: actual.bundleId, process: actual.process };
      result.matchesReceipt = actual.bundleId === receipt.build.bundleId;
      if (receipt.status === 'staged') result.activation = awaitingRestart ? 'awaiting-user-restart' : 'awaiting-finalize';
    } catch (error) { result.matchesReceipt = false; result.drift = error.message; process.exitCode = 1; }
  }
  try {
    const health = await request(opts);
    result.health = Object.fromEntries(['status', 'version', 'pid', 'accepting_turns', 'active_http_turns', 'active_browser_turns'].map(key => [key, health[key]]));
    if (!result.activeReceipt && await exists(join(opts.source, 'launcher/electron/runtime-install.cjs'))) {
      try {
        const actual = await runningIdentity(opts, opts.source, health);
        result.running = { version: actual.version, bundleId: actual.bundleId, runtimeRoot: actual.runtimeRoot, process: actual.process };
      } catch (error) { result.runtimeError = error.message; }
    }
    const source = state?.source || opts.source;
    if (await exists(join(source, 'package.json'))) result.bun = await bundledBun(opts, { source, version: health.version });
  } catch (error) { result.healthError = error.message; }
  result.entrypoints = { launcher: p.launcher, cli: p.cli };
  if (await exists(p.cli) && (await fs.lstat(p.cli)).isSymbolicLink()) result.entrypoints.cliSymlink = await fs.readlink(p.cli);
  console.log(JSON.stringify(result, null, 2));
}
export async function main(argv = process.argv.slice(2)) {
  const opts = options(argv);
  assert(process.platform === 'linux', 'This local AppImage maintenance script supports Linux only.');
  if (opts.command === 'help') {
    console.log('Commands: status | prepare --version X.Y.Z | verify [--source PATH] --browser PATH --electron PATH | build | stage | finalize | install | rollback [--receipt PATH]\nPaths: --source --patch --state-dir --core-home --install-root --bin-dir. See README.md.');
    return;
  }
  if (opts.command === 'status') return status(opts);
  if (opts.command === 'prepare') assert(opts.version, 'prepare requires explicit --version X.Y.Z.');
  return lock(opts['state-dir'], () => ['install', 'stage', 'finalize', 'rollback'].includes(opts.command)
    ? lock(join(opts['install-root'], 'local'), () => ({ install, stage, finalize, rollback }[opts.command](opts)))
    : ({ prepare, verify, build }[opts.command](opts)));
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch(error => { console.error(`upgrade: ${error.message}`); process.exitCode = 1; });
}
