import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { homedir } from 'node:os';

export function processIdentity(pid, proc = '/proc') {
  try {
    const stat = fs.readFileSync(`${proc}/${pid}/stat`, 'utf8');
    const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
    if (fields[0] === 'Z') return undefined;
    return { pid: Number(pid), parent: Number(fields[1]), startTime: fields[19] };
  } catch { return undefined; }
}

export function interactiveCodex(argv) {
  if (!/^codex(?:-[\w-]+)?$/.test(path.basename(argv[0] ?? ''))) return false;
  // Parse options before deciding whether a positional token is a subcommand.
  const values = new Set(['-c', '--config', '-m', '--model', '-p', '--profile', '-C', '--cd',
    '-s', '--sandbox', '-a', '--ask-for-approval', '-i', '--image', '--add-dir', '--enable', '--disable']);
  for (let i = 1; i < argv.length; i++) {
    const arg = argv[i];
    if (values.has(arg)) { i++; continue; }
    if (arg.startsWith('-')) continue;
    return !['exec', 'e', 'review', 'app-server', 'mcp', 'mcp-server', 'login', 'logout', 'doctor',
      'completion', 'debug', 'apply', 'a', 'cloud', 'features', 'help'].includes(arg);
  }
  return true;
}

export function findCodexHost(paneId, pid = process.ppid, proc = '/proc') {
  const seen = new Set();
  while (pid > 1 && !seen.has(pid)) {
    seen.add(pid);
    const identity = processIdentity(pid, proc);
    if (!identity) return undefined;
    try {
      const argv = fs.readFileSync(`${proc}/${pid}/cmdline`, 'utf8').split('\0').filter(Boolean);
      if (/^codex(?:-[\w-]+)?$/.test(path.basename(argv[0] ?? ''))) {
        const env = fs.readFileSync(`${proc}/${pid}/environ`, 'utf8').split('\0');
        // The nearest Codex must itself be interactive; never walk through exec/review to a parent TUI.
        return interactiveCodex(argv) && env.includes(`HERDR_PANE_ID=${paneId}`) ? identity : undefined;
      }
    } catch { return undefined; }
    pid = identity.parent;
  }
  return undefined;
}

export function paneStateFile(paneId) {
  const root = process.env.XDG_RUNTIME_DIR || `/run/user/${process.getuid()}`;
  return path.join(root, 'herdr-tps', `codex-${createHash('sha256').update(paneId).digest('hex').slice(0, 16)}.json`);
}
// flock locks the shared open file description (fd 3 in the helper). Keep
// the inode in place so concurrent hooks and sidecars serialize on one lock.
export function withPaneLock(file, action) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const fd = fs.openSync(`${file}.flock`, 'a', 0o600);
  try {
    const lock = spawnSync('flock', ['-x', '3'], { stdio: ['ignore', 'ignore', 'ignore', fd] });
    if (lock.status !== 0) throw lock.error ?? new Error('Cannot lock pane state');
    return action();
  } finally {
    fs.closeSync(fd);
  }
}

export function writeState(file, state) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const temp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temp, `${JSON.stringify(state)}\n`, { mode: 0o600 });
  fs.renameSync(temp, file);
}
export function initialModel() {
  try {
    const config = fs.readFileSync(path.join(process.env.CODEX_HOME || path.join(homedir(), '.codex'), 'config.toml'), 'utf8');
    const top = config.split(/^\s*\[/m)[0];
    const value = /^model\s*=\s*("(?:[^"\\]|\\.)*"|'[^']*')/m.exec(top)?.[1];
    return value?.startsWith('"') ? JSON.parse(value) : value?.slice(1, -1);
  } catch { return undefined; }
}
