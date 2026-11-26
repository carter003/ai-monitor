#!/usr/bin/env node
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import { HerdrMetadataPublisher } from './lib/herdr-metadata-publisher.mjs';
import { TokenRateMeter } from './lib/token-rate-meter.mjs';
import { CodexRolloutFollower } from './lib/codex-rollout-follower.mjs';
import { processIdentity, initialModel, withPaneLock, writeState } from './lib/codex-host.mjs';

export function startSidecar(file, { publisher = new HerdrMetadataPublisher({ agent: 'codex' }), autoStart = true } = {}) {
  let state, follower, inode, offset = 0, model, rate = 0, stopping = false, dirty = false;
  const meter = new TokenRateMeter({ minTokens: 0, minTimeMs: 0 });
  const snapshot = () => {
    dirty = false;
    return publisher.publishSnapshot({ model, rate }, 5000);
  };
  const makeFollower = () => new CodexRolloutFollower({
    onModel(value) { if (value) { model = value; dirty = true; } },
    onUsage(tokens, duration) {
      meter.observe(tokens, duration);
      rate = Number((meter.rate() ?? 0).toFixed(1));
      dirty = true;
    },
  });
  const readState = () => JSON.parse(fs.readFileSync(file, 'utf8'));
  const shutdown = async () => {
    if (stopping) return;
    stopping = true;
    clearInterval(poll);
    clearInterval(heartbeat);
    process.removeListener('SIGWINCH', tick);
    process.removeListener('SIGTERM', shutdown);
    process.removeListener('SIGINT', shutdown);
    try {
      const clearing = withPaneLock(file, () => {
        const current = readState();
        if (current.sidecarPid !== process.pid) return;
        writeState(file, { ...current, stopping: true });
        return { sessionId: current.sessionId, done: publisher.clearAll() };
      });
      if (!clearing) return;
      await clearing.done;
      withPaneLock(file, () => {
        const current = readState();
        if (current.sidecarPid === process.pid && current.stopping && current.sessionId === clearing.sessionId) {
          fs.unlinkSync(file);
        }
      });
    } catch { /* Already removed. */ }
  };
  const tick = () => {
    if (stopping) return;
    try {
      // The spawning hook holds this lock until it commits the child's PID.
      const next = state ? readState() : withPaneLock(file, readState);
      if (next.sidecarPid !== process.pid) return void shutdown();
      if (next.ending || processIdentity(next.codexPid)?.startTime !== next.codexStartTime) return void shutdown();
      if (!state || next.sessionId !== state.sessionId || next.transcriptPath !== state.transcriptPath) {
        state = next;
        offset = 0; inode = undefined; rate = 0; meter.reset();
        model = next.model || initialModel();
        follower = makeFollower();
        void publisher.resetForAgent(5000);
        dirty = true;
      }
      const fd = fs.openSync(state.transcriptPath, 'r');
      try {
        const stat = fs.fstatSync(fd);
        if ((inode !== undefined && inode !== stat.ino) || stat.size < offset) {
          offset = 0;
          follower = makeFollower(); meter.reset(); rate = 0; dirty = true;
        }
        inode = stat.ino;
        while (offset < stat.size) {
          const buffer = Buffer.alloc(Math.min(stat.size - offset, 65536));
          const count = fs.readSync(fd, buffer, 0, buffer.length, offset);
          if (!count) break;
          offset += count;
          follower.append(buffer.subarray(0, count));
        }
      } finally { fs.closeSync(fd); }
      if (dirty) void snapshot();
    } catch (error) {
      // A missing transcript can be transient; a missing state must end ownership.
      if (error.code !== 'ENOENT' || !fs.existsSync(file)) void shutdown();
      else if (dirty) void snapshot();
    }
  };
  const poll = autoStart ? setInterval(tick, 1000) : undefined;
  const heartbeat = autoStart ? setInterval(() => { if (!stopping) void snapshot(); }, 2000) : undefined;
  process.on('SIGWINCH', tick);
  process.once('SIGTERM', shutdown);
  process.once('SIGINT', shutdown);
  tick();
  return { tick, shutdown };
}
if (process.argv[1] === fileURLToPath(import.meta.url)) startSidecar(process.argv[2]);
