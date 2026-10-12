// Loaded by OMP's extension loader, which supplies the binary's own modules.
import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import path from 'node:path';
import { AuthStorage } from '@oh-my-pi/pi-ai';
import { SessionManager } from '@oh-my-pi/pi-coding-agent';
import { Tokenizer } from '@oh-my-pi/pi-agent-core';
import { registerOmpTpsHandlers } from '../../omp-extension.mjs';
import { OmpTpsReporter } from '../../lib/omp-tps-reporter.mjs';
import {
  ompApiKeySelectionEntryType,
  registerOmpApiKeyObserver,
} from '../../lib/omp-api-key-observer.mjs';

function eventBus(manager, sdk) {
  const handlers = new Map();
  return {
    pi: sdk,
    on(name, handler) {
      const list = handlers.get(name) ?? [];
      list.push(handler);
      handlers.set(name, list);
    },
    appendEntry: (type, data) => manager.appendCustomEntry(type, data),
    async emit(name, context, event = {}) {
      for (const handler of handlers.get(name) ?? []) await handler(event, context);
    },
  };
}

function assistant(output, duration, timestamp) {
  return {
    role: 'assistant', content: [{ type: 'text', text: 'contract fixture' }],
    api: 'openai-completions', provider: 'opencode-go', model: 'contract-model',
    timestamp, duration, stopReason: 'stop',
    usage: {
      input: 1, output, cacheRead: 0, cacheWrite: 0, totalTokens: output + 1,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
  };
}

function tpsReporter() {
  const publisher = {
    resetForAgent: async () => {}, publishModel: async () => {},
    publishDisplayAgent: async () => {}, publishRate: async () => {},
    clearModel: async () => {}, clearAll: async () => {},
  };
  return new OmpTpsReporter({ publisher, Tokenizer, autoStart: false });
}

export default function contractProbe(pi) {
  pi.on('session_start', async (_event, context) => {
    const checks = [];
    const root = process.env.HERDR_TPS_OMP_CONTRACT_ROOT;
    let auth;
    let policyAuth;
    let externalAuth;
    const reporters = [];
    const managers = [];
    try {
      auth = await AuthStorage.create(path.join(root, 'auth.db'));
      await auth.credentials.set('opencode-go', {
        type: 'api_key', key: 'herdr-contract-fake-key', source: 'login',
      });
      const id = auth.credentials.list('opencode-go')[0].id;
      const restricted = SessionManager.inMemory(root);
      const restrictedId = restricted.getSessionId();
      restricted.appendCustomEntry(ompApiKeySelectionEntryType, {
        action: 'pin', provider: 'opencode-go', sessionId: restrictedId, credentialId: id,
      });
      auth.sessions.restrict('opencode-go', restrictedId, []);
      const nativeGet = auth.keys.getWithCredential.bind(auth.keys);
      await assert.rejects(nativeGet('opencode-go', restrictedId), { name: 'MissingApiKeyError' });
      assert.equal(typeof auth.sessions.permits, 'function');
      assert.equal(auth.sessions.permits('opencode-go', restrictedId, id), false);
      const restrictedBus = eventBus(restricted);
      registerOmpApiKeyObserver(restrictedBus);
      await restrictedBus.emit('session_start', {
        modelRegistry: { authStorage: auth }, sessionManager: restricted,
      });
      await assert.rejects(auth.keys.getWithCredential('opencode-go', restrictedId), {
        name: 'MissingApiKeyError',
      });
      await restrictedBus.emit('session_shutdown');
      checks.push('restricted stored pin delegates to native auth');

      const sessionDir = path.join(root, 'sessions');
      let manager = SessionManager.create(root, sessionDir);
      managers.push(manager);
      manager.appendMessage({ role: 'user', content: 'first', timestamp: 1000 });
      const leaf = manager.appendMessage(assistant(3000, 10000, 2000));
      const bus = eventBus(manager);
      registerOmpApiKeyObserver(bus);
      const authContext = { modelRegistry: { authStorage: auth }, sessionManager: manager };
      await bus.emit('session_start', authContext);
      const parentId = manager.getSessionId();
      await auth.keys.getWithCredential('opencode-go', parentId);
      assert.equal(auth.sessions.permits('opencode-go', parentId, id), true);
      manager.createBranchedSession(leaf);
      const branchId = manager.getSessionId();
      assert.notEqual(branchId, parentId);
      await bus.emit('session_branch', authContext);
      await auth.keys.getWithCredential('opencode-go', branchId);
      const branchPins = manager.getEntries().filter((entry) =>
        entry.customType === ompApiKeySelectionEntryType && entry.data.sessionId === branchId);
      assert.equal(branchPins.length, 1);
      assert.equal(branchPins[0].data.credentialId, id);
      await bus.emit('session_shutdown');

      // Reopen both persisted stores; resuming must not rank credentials again.
      const branchFile = manager.getSessionFile();
      assert.equal(typeof branchFile, 'string');
      await manager.flush();
      await manager.close();
      manager = await SessionManager.open(branchFile, sessionDir);
      managers.push(manager);
      assert.equal(manager.getSessionId(), branchId);
      auth.close();
      auth = await AuthStorage.create(path.join(root, 'auth.db'));
      await auth.credentials.reload();
      auth.keys.getWithCredential = async () => { throw new Error('resume unexpectedly ranked'); };
      const resumed = eventBus(manager);
      registerOmpApiKeyObserver(resumed);
      await resumed.emit('session_start', {
        modelRegistry: { authStorage: auth }, sessionManager: manager,
      });
      assert.equal((await auth.keys.getWithCredential('opencode-go', branchId)).credentialId, id);
      await resumed.emit('session_shutdown');
      checks.push('branch writes its own pin and resumes without ranking');

      manager.appendMessage({ role: 'user', content: 'second', timestamp: 12000 });
      manager.appendMessage(assistant(9000, 1000, 13000));
      const tpsBus = eventBus(manager, pi.pi);
      const reporter = tpsReporter();
      reporters.push(reporter);
      registerOmpTpsHandlers(tpsBus, () => reporter, { requireUi: true });
      const ui = {
        hasUI: true, agent: { kind: 'main' },
        model: { id: 'contract-model' }, sessionManager: manager,
      };
      await tpsBus.emit('session_start', ui);
      assert.equal(reporter.lastRate, 9000);
      manager.branch(leaf);
      await tpsBus.emit('session_tree', ui);
      assert.equal(reporter.lastRate, 300);
      await tpsBus.emit('session_shutdown');
      checks.push('tree seeds TPS from the active branch');

      const delayedBus = eventBus(manager, pi.pi);
      const delayedReporter = tpsReporter();
      reporters.push(delayedReporter);
      registerOmpTpsHandlers(delayedBus, () => delayedReporter, { requireUi: true });
      await delayedBus.emit('session_start', { ...ui, hasUI: false });
      await delayedBus.emit('session_switch', ui);
      assert.equal(delayedReporter.currentModel, 'contract-model');
      assert.equal(delayedReporter.lastRate, 300);
      checks.push('first UI switch retains the model and seeded TPS');
      manager.appendResetBoundary();
      await delayedBus.emit('session_tree', ui);
      assert.equal(delayedReporter.lastRate, 0);
      assert.equal(delayedReporter.currentModel, 'contract-model');
      await delayedBus.emit('session_shutdown');
      checks.push('reset boundary excludes earlier TPS history');
      manager.appendMessage(assistant(1200, 2000, 14000));
      const retained = manager.appendMessage({ role: 'user', content: 'after compaction', timestamp: 16000 });
      manager.appendCompaction('contract summary', undefined, retained, 2000);
      const compactedBus = eventBus(manager, pi.pi);
      const compactedReporter = tpsReporter();
      reporters.push(compactedReporter);
      registerOmpTpsHandlers(compactedBus, () => compactedReporter, { requireUi: true });
      await compactedBus.emit('session_start', ui);
      assert.equal(compactedReporter.lastRate, 0);
      await compactedBus.emit('session_shutdown');
      checks.push('native context builder excludes compacted assistant history');

      const policyDb = path.join(root, 'policy-auth.db');
      policyAuth = await AuthStorage.create(policyDb);
      await policyAuth.credentials.set('opencode-go', {
        type: 'api_key', key: 'contract-stored-key',
      });
      const policyManager = SessionManager.inMemory(root);
      const policyId = policyManager.getSessionId();
      const policyBus = eventBus(policyManager);
      registerOmpApiKeyObserver(policyBus);
      await policyBus.emit('session_start', {
        modelRegistry: { authStorage: policyAuth }, sessionManager: policyManager,
      });
      assert.equal((await policyAuth.keys.getWithCredential('opencode-go', policyId)).apiKey, 'contract-stored-key');
      policyAuth.keys.setRuntime('opencode-go', 'contract-runtime-key');
      assert.equal((await policyAuth.keys.getWithCredential('opencode-go', policyId)).apiKey, 'contract-runtime-key');
      policyAuth.keys.removeRuntime('opencode-go');
      assert.equal((await policyAuth.keys.getWithCredential('opencode-go', policyId)).apiKey, 'contract-stored-key');
      const storedId = policyAuth.credentials.list('opencode-go')[0].id;
      await policyAuth.credentials.set('opencode-go', [
        { type: 'api_key', key: 'contract-stored-key' },
        { type: 'api_key', key: 'contract-login-key', source: 'login' },
      ]);
      assert.ok(policyAuth.credentials.list('opencode-go').some((entry) => entry.id === storedId));
      assert.equal((await policyAuth.keys.getWithCredential('opencode-go', policyId)).apiKey, 'contract-login-key');
      externalAuth = await AuthStorage.create(policyDb);
      await externalAuth.credentials.set('opencode-go', {
        type: 'api_key', key: 'contract-external-key', source: 'login',
      });
      externalAuth.close();
      externalAuth = undefined;
      assert.equal((await policyAuth.keys.getWithCredential('opencode-go', policyId)).apiKey, 'contract-external-key');
      await policyBus.emit('session_shutdown');
      checks.push('native overrides, login priority and external auth writes supersede cached keys');
      await writeFile(process.env.HERDR_TPS_OMP_CONTRACT_RESULT, JSON.stringify({ checks }));
    } catch (error) {
      await writeFile(process.env.HERDR_TPS_OMP_CONTRACT_RESULT, JSON.stringify({
        checks, error: error.stack ?? String(error),
      }));
    } finally {
      for (const reporter of reporters) await reporter.close();
      for (const manager of managers) await manager.close();
      auth?.close();
      policyAuth?.close();
      externalAuth?.close();
      context.shutdown();
    }
  });
}
