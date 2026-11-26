#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { findCodexHost, paneStateFile, processIdentity, withPaneLock, writeState } from './lib/codex-host.mjs';

export function runHook(input, event = process.argv[2]) {
  const env = process.env;
  if (env.HERDR_ENV !== '1' || !env.HERDR_PANE_ID || !env.HERDR_SOCKET_PATH ||
      env.OMPCODE === '1' || Boolean(env.HERDR_TPS_OMP_TOPLEVEL) ||
      (env.CODEX_THREAD_ID && env.CODEX_THREAD_ID !== input.session_id)) return;
  if (!input.session_id || !path.isAbsolute(input.transcript_path ?? '') || !fs.existsSync(input.transcript_path)) return;
  const host = findCodexHost(env.HERDR_PANE_ID);
  if (!host) return;
  const file = paneStateFile(env.HERDR_PANE_ID);
  withPaneLock(file, () => {
    let previous;
    try { previous = JSON.parse(fs.readFileSync(file, 'utf8')); } catch { /* New pane. */ }
    const sameHost = previous?.codexPid === host.pid && previous?.codexStartTime === host.startTime;
    const sidecar = processIdentity(previous?.sidecarPid);
    const alive = sidecar && sidecar.startTime === previous?.sidecarStartTime;
    if (event === 'end') {
      if (sameHost && alive && previous.sessionId === input.session_id) {
        writeState(file, { ...previous, ending: true });
        process.kill(sidecar.pid, 'SIGWINCH');
      }
      return;
    }
    if (previous && !sameHost && processIdentity(previous.codexPid)?.startTime === previous.codexStartTime) return;
    const state = { paneId: env.HERDR_PANE_ID, codexPid: host.pid, codexStartTime: host.startTime,
      sessionId: input.session_id, transcriptPath: input.transcript_path, model: input.model,
      updatedAt: Date.now() };
    if (sameHost && alive && !previous.ending && !previous.stopping) {
      writeState(file, { ...state, sidecarPid: sidecar.pid, sidecarStartTime: sidecar.startTime });
      process.kill(sidecar.pid, 'SIGWINCH');
    } else {
      const childEnv = { ...env };
      delete childEnv.CODEX_THREAD_ID; // Session ownership was validated above.
      const child = spawn(process.execPath, [fileURLToPath(new URL('./codex-herdr-sidecar.js', import.meta.url)), file],
        { detached: true, stdio: 'ignore', env: childEnv });
      child.unref();
      const identity = processIdentity(child.pid);
      writeState(file, { ...state, sidecarPid: child.pid, sidecarStartTime: identity?.startTime });
    }
  });
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try { runHook(JSON.parse(fs.readFileSync(0, 'utf8'))); } catch { /* Hooks must never prevent a session opening. */ }
  process.exit(0);
}
