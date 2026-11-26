#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { findCodexHost, paneStateFile, processIdentity, writeState } from './lib/codex-host.mjs';

export function runHook(input, event = process.argv[2]) {
  const env = process.env;
  if (env.HERDR_ENV !== '1' || !env.HERDR_PANE_ID || !env.HERDR_SOCKET_PATH ||
      env.OMPCODE === '1' || Boolean(env.HERDR_TPS_OMP_TOPLEVEL) ||
      (env.CODEX_THREAD_ID && env.CODEX_THREAD_ID !== input.session_id)) return;
  if (!input.session_id || !path.isAbsolute(input.transcript_path ?? '') || !fs.existsSync(input.transcript_path)) return;
  const host = findCodexHost(env.HERDR_PANE_ID);
  if (!host) return;
  const file = paneStateFile(env.HERDR_PANE_ID);
  // Serialize concurrent hooks; never start a second observer for this pane.
  const lock = `${file}.lock`;
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  let fd;
  try { fd = fs.openSync(lock, 'wx', 0o600); }
  catch {
    // Recover only a lock whose owner identity is provably gone.
    try {
      const owner = JSON.parse(fs.readFileSync(lock, 'utf8'));
      if (processIdentity(owner.pid)?.startTime === owner.startTime) return;
      fs.unlinkSync(lock);
      fd = fs.openSync(lock, 'wx', 0o600);
    } catch { return; }
  }
  fs.writeFileSync(fd, JSON.stringify(processIdentity(process.pid)));
  try {
    let previous;
    try { previous = JSON.parse(fs.readFileSync(file, 'utf8')); } catch { /* New pane. */ }
    const sameHost = previous?.codexPid === host.pid && previous?.codexStartTime === host.startTime;
    const sidecar = processIdentity(previous?.sidecarPid);
    const alive = sidecar && sidecar.startTime === previous?.sidecarStartTime;
    if (event === 'end') {
      if (sameHost && alive && previous.sessionId === input.session_id) {
        writeState(file, { ...previous, ending: true });
        process.kill(sidecar.pid, 'SIGUSR1');
      }
      return;
    }
    if (previous && !sameHost && processIdentity(previous.codexPid)?.startTime === previous.codexStartTime) return;
    const state = { paneId: env.HERDR_PANE_ID, codexPid: host.pid, codexStartTime: host.startTime,
      sessionId: input.session_id, transcriptPath: input.transcript_path, model: input.model,
      updatedAt: Date.now() };
    if (sameHost && alive) {
      writeState(file, { ...state, sidecarPid: sidecar.pid, sidecarStartTime: sidecar.startTime });
      process.kill(sidecar.pid, 'SIGUSR1');
    } else {
      writeState(file, state);
      const childEnv = { ...env };
      delete childEnv.CODEX_THREAD_ID; // Session ownership was validated above.
      const child = spawn(process.execPath, [fileURLToPath(new URL('./codex-herdr-sidecar.js', import.meta.url)), file],
        { detached: true, stdio: 'ignore', env: childEnv });
      child.unref();
      const identity = processIdentity(child.pid);
      writeState(file, { ...state, sidecarPid: child.pid, sidecarStartTime: identity?.startTime });
    }
  } finally { fs.closeSync(fd); fs.unlinkSync(lock); }
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try { runHook(JSON.parse(fs.readFileSync(0, 'utf8'))); } catch { /* Hooks must never prevent a session opening. */ }
  process.exit(0);
}
