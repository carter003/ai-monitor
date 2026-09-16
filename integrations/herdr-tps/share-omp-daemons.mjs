#!/usr/bin/env node

// One-time host configuration: preserve pro2 settings/auth/sessions, share only
// its daemon namespace with the default profile. Never stop a running client.
import { randomUUID } from 'node:crypto';
import {
  lstat,
  mkdir,
  readdir,
  readFile,
  realpath,
  rename,
  rmdir,
  symlink,
} from 'node:fs/promises';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

async function optionalStat(file) {
  try {
    return await lstat(file);
  } catch (error) {
    if (error.code === 'ENOENT') return undefined;
    throw error;
  }
}

async function entries(directory) {
  if (!(await optionalStat(directory))) return [];
  return readdir(directory, { withFileTypes: true });
}

function alive(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) throw new Error('Invalid runtime PID');
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error.code === 'ESRCH') return false;
    throw error;
  }
}

async function liveRuntimePids(source) {
  const live = new Set();
  for (const scope of await entries(source)) {
    if (!scope.isDirectory()) continue;
    const directory = path.join(source, scope.name);
    const records = [path.join(directory, 'broker.pid')];
    for (const client of await entries(path.join(directory, 'clients'))) {
      if (client.isFile() && client.name.endsWith('.json')) {
        records.push(path.join(directory, 'clients', client.name));
      }
    }
    for (const daemon of await entries(path.join(directory, 'daemons'))) {
      if (daemon.isDirectory())
        records.push(path.join(directory, 'daemons', daemon.name, 'meta.json'));
    }
    for (const file of records) {
      if (!(await optionalStat(file))) continue;
      const record = JSON.parse(await readFile(file, 'utf8'));
      const pid = record.daemon ? record.daemon.pid : record.pid;
      if (pid !== undefined && alive(pid)) live.add(pid);
    }
  }
  return [...live].sort((a, b) => a - b);
}

export async function shareOmpDaemons({ configRoot, apply = false }) {
  const root = path.resolve(configRoot);
  const source = path.join(root, 'profiles', 'pro2', 'run', 'daemons');
  const target = path.join(root, 'run', 'daemons');
  const sourceStat = await optionalStat(source);
  if (sourceStat?.isSymbolicLink()) {
    if ((await realpath(source)) !== (await realpath(target))) {
      throw new Error(`Existing daemon link points elsewhere: ${source}`);
    }
    return { status: 'already-shared', source, target };
  }
  if (sourceStat && !sourceStat.isDirectory()) throw new Error(`Not a daemon directory: ${source}`);
  const pids = await liveRuntimePids(source);
  if (pids.length) return { status: 'blocked-live-runtime', source, target, pids };
  if (!apply) return { status: 'ready', source, target };

  await mkdir(path.dirname(source), { recursive: true, mode: 0o700 });
  const lock = path.join(path.dirname(source), '.daemon-sharing.lock');
  await mkdir(lock, { mode: 0o700 });
  try {
    const current = await optionalStat(source);
    if (current?.ino !== sourceStat?.ino || current?.dev !== sourceStat?.dev) {
      throw new Error(
        'Daemon directory changed concurrently; retry after the other launcher finishes',
      );
    }
    const currentPids = await liveRuntimePids(source);
    if (currentPids.length)
      return { status: 'blocked-live-runtime', source, target, pids: currentPids };
    await mkdir(target, { recursive: true, mode: 0o700 });
    const backup = sourceStat ? `${source}.before-share-${Date.now()}-${randomUUID()}` : undefined;
    if (backup) await rename(source, backup);
    try {
      await symlink(path.relative(path.dirname(source), target), source, 'dir');
    } catch (error) {
      if (backup) await rename(backup, source);
      throw error;
    }
    return { status: 'shared', source, target, backup };
  } finally {
    await rmdir(lock);
  }
}

export async function shareLocalOmpDaemons({ apply = false } = {}) {
  const root = path.join(homedir(), process.env.PI_CONFIG_DIR || '.omp');
  if (
    process.env.XDG_STATE_HOME &&
    (await optionalStat(path.join(process.env.XDG_STATE_HOME, 'omp')))
  ) {
    throw new Error('Active XDG state layout requires a separate runtime-path review');
  }
  return shareOmpDaemons({ configRoot: root, apply });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    if (args.some((argument) => argument !== '--apply' && argument !== '--dry-run')) {
      throw new Error(
        'Usage: node integrations/herdr-tps/share-omp-daemons.mjs [--dry-run | --apply]',
      );
    }
    if (args.includes('--apply') && args.includes('--dry-run')) throw new Error('Choose one mode');
    const result = await shareLocalOmpDaemons({ apply: args.includes('--apply') });
    console.log(JSON.stringify(result, null, 2));
    if (result.status === 'blocked-live-runtime') {
      console.error(
        'Exit all omp2/pro2 clients normally, wait for their broker to exit, then retry. No runtime was changed.',
      );
      process.exitCode = 2;
    }
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
