import assert from 'node:assert/strict';
import test from 'node:test';
import {
  ompApiKeySelectionEntryType,
  registerOmpApiKeyObserver,
} from '../lib/omp-api-key-observer.mjs';

function fixture({ entries = [], selected = ['account-a', 'account-a', 'account-b'] } = {}) {
  const handlers = new Map();
  const appended = [];
  let calls = 0;
  const listeners = new Set();
  const blocked = new Map();
  let sessionId = 'session-1';
  let rotation = { switched: true };
  const namespaces = (accounts) => ({
    credentials: {
      onGeneration(listener) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      list(provider) {
        if (provider !== 'opencode-go') return [];
        return [...new Set(accounts)].map((apiKey) => ({
          id: apiKey === 'account-a' ? 7 : 8,
          provider,
          credential: { type: 'api_key', key: apiKey },
        }));
      },
    },
    blocks: {
      list(ids) {
        return ids.flatMap((id) => blocked.has(id) ? [{
          credentialId: id,
          providerKey: 'opencode-go:api_key',
          blockedUntilMs: blocked.get(id),
        }] : []);
      },
    },
    keys: {
      getWithCredential: async () => {
        const apiKey = accounts[Math.min(calls++, accounts.length - 1)];
        return { apiKey, credentialId: apiKey === 'account-a' ? 7 : 8 };
      },
    },
    sessions: { release: () => true },
    limits: {
      markReached: async () => ({ switched: true }),
      rotate: async () => rotation,
    },
  });
  const notifyGeneration = () => {
    for (const listener of listeners) listener();
  };
  const auth = {
    ...namespaces(selected),
    async replaceStore(accounts, error) {
      // Match OMP: load first, swap all namespaces, then notify the subscribers
      // adopted from the previous credential pool. Failed loads keep the store.
      await Promise.resolve();
      if (error) throw error;
      Object.assign(this, namespaces(accounts));
      notifyGeneration();
    },
  };
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
    appendEntry: (customType, data) => appended.push({ type: 'custom', customType, data }),
  };
  const sessionManager = {
    getEntries: () => [...entries, ...appended],
    getSessionId: () => sessionId,
    appendResetBoundary: () => ({ type: 'reset_boundary' }),
  };
  const state = registerOmpApiKeyObserver(pi);
  const activate = (event = 'session_start') => handlers.get(event)({}, {
    modelRegistry: { authStorage: auth },
    sessionManager,
  });
  activate();
  return {
    auth, appended, calls: () => calls, activate, notifyGeneration, listeners, pi, state,
    sessionManager,
    setSessionId: (value) => { sessionId = value; },
    setRotation: (value) => { rotation = value; },
    block: (id) => blocked.set(id, Date.now() + 60_000),
  };
}

test('ranks once and keeps the selected API key for the whole session', async () => {
  const f = fixture();
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), {
    apiKey: 'account-a',
    credentialId: 7,
  });
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), {
    apiKey: 'account-a',
    credentialId: 7,
  });
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), {
    apiKey: 'account-a',
    credentialId: 7,
  });

  assert.equal(f.calls(), 1, 'ordinary requests must not re-run usage ranking');
  assert.deepEqual(
    f.appended.map((entry) => [entry.customType, entry.data.action, entry.data.credentialId]),
    [
      [ompApiKeySelectionEntryType, 'pin', 7],
    ],
  );
  assert.deepEqual(f.appended.map((entry) => entry.data.reason), ['initial']);
  assert.equal(JSON.stringify(f.appended).includes('account-a'), false);
  assert.equal(JSON.stringify(f.appended).includes('account-b'), false);
});

test('a new session runs ranking independently', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
  f.setSessionId('session-2');
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-2')).credentialId, 8);
  assert.equal(f.calls(), 2);
});

test('concurrent first requests share one ranking decision', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  const [first, second] = await Promise.all([
    f.auth.keys.getWithCredential('opencode-go', 'session-1'),
    f.auth.keys.getWithCredential('opencode-go', 'session-1'),
  ]);
  assert.equal(first.credentialId, 7);
  assert.equal(second.credentialId, 7);
  assert.equal(f.calls(), 1);
});

test('reset releases the current session pin and ranks again', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
  f.sessionManager.appendResetBoundary();
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  assert.equal(f.calls(), 2);
  assert.deepEqual(f.appended.map(({ data }) => [data.action, data.reason]), [
    ['pin', 'initial'], ['release', 'reset'], ['pin', 'reset'],
  ]);
});

test('an active block invalidates the pin and permits failover ranking', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
  f.block(7);
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  assert.equal(f.calls(), 2);
  assert.equal(f.appended.at(-1).data.reason, 'blocked');
});

test('records explicit release boundaries and allows the next request to rank', async () => {
  const f = fixture();
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.equal(f.auth.sessions.release('opencode-go', 'session-1'), true);
  assert.deepEqual(f.appended.map((entry) => entry.data.action), ['pin', 'release']);
});

test('carries an explicit release reason onto the next selected account', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  f.auth.sessions.release('opencode-go', 'session-1');
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.equal(f.appended.at(-1).data.reason, 'reselection');
  assert.equal(f.appended.at(-1).data.previousCredentialId, 7);
});

test('restores observed state and avoids duplicate records after restart', async () => {
  const entries = [
    {
      type: 'custom',
      customType: ompApiKeySelectionEntryType,
      data: {
        action: 'pin',
        provider: 'opencode-go',
        sessionId: 'session-1',
        credentialId: 7,
      },
    },
  ];
  const f = fixture({ entries, selected: ['account-a'] });
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), {
    apiKey: 'account-a',
    credentialId: 7,
  });
  assert.deepEqual(f.appended, []);
  assert.equal(f.calls(), 0, 'resume restores the durable credential without ranking');
});

test('observes new namespaces immediately across repeated store replacements', async () => {
  const f = fixture({ selected: ['account-a'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await f.auth.replaceStore(['account-b']);
  assert.equal(f.state.selections.size, 0);
  assert.equal(f.appended.at(-1).data.reason, 'store-replaced');
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  assert.equal(f.auth.sessions.release('opencode-go', 'session-1'), true);
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.deepEqual(await f.auth.limits.markReached('opencode-go', 'session-1'), { switched: true });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.deepEqual(await f.auth.limits.rotate('opencode-go', 'session-1'), { switched: true });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await f.auth.replaceStore(['account-a']);
  f.activate('session_switch');
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.deepEqual(f.appended.map(({ data }) => [data.action, data.credentialId ?? data.reason]), [
    ['pin', 7], ['release', 'store-replaced'], ['pin', 8], ['release', 'reselection'],
    ['pin', 8], ['release', 'usage-limit'], ['pin', 8], ['release', 'rotation'],
    ['pin', 8], ['release', 'store-replaced'], ['pin', 7],
  ]);
});

test('a rotation without an available sibling keeps the pin', async () => {
  const f = fixture({ selected: ['account-a'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  f.setRotation({ switched: false, afterSiblingWait: true });
  const rotated = await f.auth.limits.rotate('opencode-go', 'session-1');
  assert.deepEqual(rotated, { switched: false, afterSiblingWait: true });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.equal(f.calls(), 1, 'the same account stays pinned until a real switch');
  assert.deepEqual(f.appended.map(({ data }) => data.action), ['pin']);
});

test('records a new pin when a replacement store reuses a credential ID', async () => {
  const f = fixture({ selected: ['account-a'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await f.auth.keys.getWithCredential('opencode-go', 'session-2');
  await f.auth.replaceStore(['account-a']);
  assert.equal(f.state.selections.size, 0);
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.deepEqual(f.appended.map(({ data }) => [data.action, data.sessionId]), [
    ['pin', 'session-1'], ['pin', 'session-2'], ['release', 'session-1'],
    ['release', 'session-2'], ['pin', 'session-1'],
  ]);
});

test('ordinary generation changes and repeated activation do not stack observers', async () => {
  const f = fixture({ selected: ['account-a'] });
  const get = f.auth.keys.getWithCredential;
  await get('opencode-go', 'session-1');
  f.notifyGeneration();
  f.activate('session_switch');
  registerOmpApiKeyObserver(f.pi);
  f.activate();
  assert.equal(f.auth.keys.getWithCredential, get);
  assert.equal(f.listeners.size, 1);
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.equal(f.appended.length, 1);
  assert.equal(f.calls(), 1);
});

test('failed replacement keeps the old observation and cached selection', async () => {
  const f = fixture({ selected: ['account-a'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  const error = new Error('store load failed');
  await assert.rejects(f.auth.replaceStore(['account-b'], error), (actual) => actual === error);
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.equal(f.appended.length, 1);
  f.auth.sessions.release('opencode-go', 'session-1');
  assert.equal(f.appended.at(-1).data.reason, 'reselection');
});

test('late operations on old namespaces cannot overwrite or release a new pin', async () => {
  const f = fixture({ selected: ['account-a'] });
  const oldKeys = f.auth.keys;
  const oldSessions = f.auth.sessions;
  const oldLimits = f.auth.limits;
  // The wrappers await the original async methods, so these finish after swap.
  // Queue replacement first to swap before the pending observers resume.
  const replacement = f.auth.replaceStore(['account-b']);
  const oldSelection = oldKeys.getWithCredential('opencode-go', 'session-1');
  const oldMark = oldLimits.markReached('opencode-go', 'session-1');
  const oldRotate = oldLimits.rotate('opencode-go', 'session-1');
  await replacement;
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await Promise.all([oldSelection, oldMark, oldRotate]);
  await oldKeys.getWithCredential('opencode-go', 'session-1');
  oldSessions.release('opencode-go', 'session-1');
  await oldLimits.markReached('opencode-go', 'session-1');
  await oldLimits.rotate('opencode-go', 'session-1');
  assert.deepEqual(f.appended.map(({ data }) => [data.action, data.credentialId]), [['pin', 8]]);
});
