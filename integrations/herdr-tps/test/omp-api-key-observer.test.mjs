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
  let nativeCalls = 0;
  const listeners = new Set();
  const blocked = new Map();
  const restricted = new Set();
  const resolutionWaits = [];
  const extraCredentials = [];
  const restrictionError = new Error('native restricted account pool');
  let authSource;
  let poll = async () => false;
  let sessionId = 'session-1';
  let rotation = { switched: true };
  let nativeRelease = true;
  const namespaces = (accounts) => ({
    credentials: {
      poll: () => poll(accounts),
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
        })).concat(extraCredentials);
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
      source: () => ({ kind: authSource?.kind ?? 'api_key', concrete: true }),
      isKeylessFallback: (_provider, credential) => credential.key === 'keyless-marker',
      getWithCredential: async (_provider, id) => {
        nativeCalls += 1;
        if (restricted.has(id)) throw restrictionError;
        if (authSource) return { apiKey: authSource.apiKey };
        const login = extraCredentials.find(({ credential, disabledCause }) =>
          disabledCause == null && credential.source === 'login' && credential.type === 'api_key' &&
          credential.key && credential.key !== 'keyless-marker');
        if (login) {
          calls += 1;
          return { apiKey: login.credential.key, credentialId: login.id };
        }
        const apiKey = accounts[Math.min(calls++, accounts.length - 1)];
        const wait = resolutionWaits.shift();
        if (wait) {
          wait.entered();
          await wait.promise;
        }
        return { apiKey, credentialId: apiKey === 'account-a' ? 7 : 8 };
      },
    },
    sessions: {
      isRestricted: (_provider, id) => restricted.has(id),
      release: () => nativeRelease,
    },
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
    auth,
    appended,
    calls: () => calls,
    nativeCalls: () => nativeCalls,
    activate,
    notifyGeneration,
    listeners,
    pi,
    state,
    sessionManager,
    shutdown: () => handlers.get('session_shutdown')?.(),
    restrict: (id) => restricted.add(id),
    restrictionError,
    setSource: (kind, apiKey = 'native-override') => { authSource = kind ? { kind, apiKey } : undefined; },
    setPoll: (value) => { poll = value; },
    addCredential: (row, notify = true) => {
      extraCredentials.push(row);
      if (notify) notifyGeneration();
    },
    deferSelection: () => {
      let resolve;
      let entered;
      const promise = new Promise((done) => { resolve = done; });
      const started = new Promise((done) => { entered = done; });
      resolutionWaits.push({ promise, entered });
      return { resolve, started };
    },
    setSessionId: (value) => { sessionId = value; },
    setRotation: (value) => { rotation = value; },
    setNativeRelease: (value) => { nativeRelease = value; },
    block: (id) => blocked.set(id, Date.now() + 60_000),
  };
}

function sessionScope(auth, id, entries = []) {
  const handlers = new Map();
  const appended = [];
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
    appendEntry: (customType, data) => appended.push({ type: 'custom', customType, data }),
  };
  const sessionManager = {
    getSessionId: () => id,
    getEntries: () => [...entries, ...appended],
    appendResetBoundary: () => ({ type: 'reset_boundary' }),
  };
  registerOmpApiKeyObserver(pi);
  const activate = () => handlers.get('session_start')({}, {
    modelRegistry: { authStorage: auth }, sessionManager,
  });
  activate();
  return { appended, activate, shutdown: () => handlers.get('session_shutdown')?.() };
}

function pin(sessionId, credentialId = 7) {
  return {
    type: 'custom',
    customType: ompApiKeySelectionEntryType,
    data: { action: 'pin', provider: 'opencode-go', sessionId, credentialId },
  };
}

test('shared auth preserves each child session account timeline', async () => {
  const parent = fixture({ selected: ['account-a', 'account-b', 'account-a'] });
  await parent.auth.keys.getWithCredential('opencode-go', 'session-1');
  const first = sessionScope(parent.auth, 'child-a');
  const second = sessionScope(parent.auth, 'child-b');
  await parent.auth.keys.getWithCredential('opencode-go', 'child-a');
  await parent.auth.keys.getWithCredential('opencode-go', 'child-b');
  parent.activate();
  await parent.auth.limits.markReached('opencode-go', 'child-a');
  await parent.auth.keys.getWithCredential('opencode-go', 'child-a');
  assert.deepEqual(parent.appended.map(({ data }) => [data.sessionId, data.credentialId]), [['session-1', 7]]);
  assert.deepEqual(first.appended.map(({ data }) => [data.sessionId, data.action, data.credentialId, data.reason]), [
    ['child-a', 'pin', 8, 'initial'],
    ['child-a', 'release', undefined, 'usage-limit'],
    ['child-a', 'pin', 7, 'usage-limit'],
  ]);
  assert.deepEqual(second.appended.map(({ data }) => [data.sessionId, data.credentialId]), [['child-b', 7]]);
});

test('a revived session keeps its writer when the old scope shuts down', async () => {
  const parent = fixture({ selected: ['account-a', 'account-b'] });
  const old = sessionScope(parent.auth, 'child-a');
  await parent.auth.keys.getWithCredential('opencode-go', 'child-a');
  const revived = sessionScope(parent.auth, 'child-a', old.appended);
  old.shutdown();
  await parent.auth.limits.markReached('opencode-go', 'child-a');
  await parent.auth.keys.getWithCredential('opencode-go', 'child-a');
  assert.deepEqual(old.appended.map(({ data }) => [data.action, data.credentialId]), [['pin', 7]]);
  assert.deepEqual(revived.appended.map(({ data }) => [data.action, data.credentialId]), [['release', undefined], ['pin', 8]]);
});

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

test('native release returning false still invalidates a local extension pin', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  f.setNativeRelease(false);
  assert.equal(f.auth.sessions.release('opencode-go', 'session-1'), false);
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  assert.deepEqual(f.appended.map(({ data }) => data.action), ['pin', 'release', 'pin']);
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

test('credential store replacement resets each session without mixing timelines', async () => {
  const f = fixture({ selected: ['account-a'] });
  const child = sessionScope(f.auth, 'session-2');
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await f.auth.keys.getWithCredential('opencode-go', 'session-2');
  await f.auth.replaceStore(['account-a']);
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.deepEqual(f.appended.map(({ data }) => [data.action, data.sessionId]), [
    ['pin', 'session-1'], ['release', 'session-1'], ['pin', 'session-1'],
  ]);
  assert.deepEqual(child.appended.map(({ data }) => [data.action, data.sessionId]), [
    ['pin', 'session-2'], ['release', 'session-2'],
  ]);
  assert.equal(f.appended.at(-1).data.reason, 'store-replaced');
  assert.equal(f.appended.at(-1).data.credentialId, 7);
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

test('native restrictions reject both live cached and restored API-key pins', async () => {
  for (const restored of [false, true]) {
    const f = fixture({ entries: restored ? [pin('session-1')] : [] });
    if (!restored) await f.auth.keys.getWithCredential('opencode-go', 'session-1');
    f.restrict('session-1');
    const previousCalls = f.nativeCalls();
    await assert.rejects(f.auth.keys.getWithCredential('opencode-go', 'session-1'), (error) => error === f.restrictionError);
    await assert.rejects(f.auth.keys.getWithCredential('opencode-go', 'session-1'), (error) => error === f.restrictionError);
    assert.equal(f.nativeCalls(), previousCalls + 2, 'native policy handles every restricted request');
    assert.equal(f.state.resolved.size, 0);
    assert.equal(f.state.selections.size, 0);
    assert.equal(f.appended.at(-1).data.reason, 'restricted');
  }
});

test('a restriction added during native selection is enforced before returning a key', async () => {
  const f = fixture();
  const pending = f.deferSelection();
  const selection = f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await pending.started;
  f.restrict('session-1');
  pending.resolve();
  await assert.rejects(selection, (error) => error === f.restrictionError);
  assert.equal(f.state.resolved.size, 0);
  assert.deepEqual(f.appended, []);
});

test('native runtime, config, env and OAuth sources take precedence over a cached API key', async () => {
  for (const kind of ['runtime', 'config', 'env', 'oauth']) {
    const f = fixture({ selected: ['account-a', 'account-b'] });
    await f.auth.keys.getWithCredential('opencode-go', 'session-1');
    f.setSource(kind);
    assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), { apiKey: 'native-override' });
    assert.equal(f.state.resolved.size, 0);
    assert.equal(f.state.selections.size, 0);
    assert.equal(f.appended.at(-1).data.reason, 'auth-source-changed');
    f.setSource();
    assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
    assert.equal(f.appended.at(-1).data.previousCredentialId, 7);
  }
});

test('an existing native override also supersedes a durable restored pin', async () => {
  const f = fixture({ entries: [pin('session-1')] });
  f.setSource('config');
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), { apiKey: 'native-override' });
  assert.equal(f.calls(), 0);
  assert.equal(f.state.selections.size, 0);
});

test('a login API key outranks both a live cached and durable non-login pin', async () => {
  for (const restored of [false, true]) {
    const f = fixture({ entries: restored ? [pin('session-1')] : [] });
    if (!restored) await f.auth.keys.getWithCredential('opencode-go', 'session-1');
    f.addCredential({ id: 9, credential: { type: 'api_key', source: 'login', key: 'login-secret' } }, restored);
    assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 9);
    assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 9);
    assert.equal(f.appended.at(-1).data.reason, 'auth-source-changed');
    assert.equal(f.appended.at(-1).data.previousCredentialId, 7);
    assert.equal(f.calls(), restored ? 1 : 2, 'the new login credential becomes sticky on its first request');
  }
});

test('disabled, empty and native keyless login markers do not invalidate an API-key pin', async () => {
  for (const candidate of [
    { id: 9, credential: { type: 'api_key', source: 'login', key: 'disabled-secret' }, disabledCause: 'disabled' },
    { id: 9, credential: { type: 'api_key', source: 'login', key: '' } },
    { id: 9, credential: { type: 'api_key', source: 'login', key: 'keyless-marker' } },
  ]) {
    for (const restored of [false, true]) {
      const f = fixture({ entries: restored ? [pin('session-1')] : [] });
      if (!restored) await f.auth.keys.getWithCredential('opencode-go', 'session-1');
      f.addCredential(candidate);
      assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
      assert.equal(f.calls(), restored ? 0 : 1);
      assert.equal(f.appended.length, restored ? 0 : 1);
    }
  }
});

test('recordAffinity false probes bypass routing without changing the session pin', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  const options = { recordAffinity: false, modelId: 'probe-model' };
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1', options)).credentialId, 8);
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
  assert.equal(f.calls(), 2);
  assert.deepEqual(f.appended.map(({ data }) => data.action), ['pin']);
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'probe-only', options)).credentialId, 8);
  assert.equal(f.state.selections.size, 1);
});

test('cached requests adopt external credential deletion before restoring a pin', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  let polls = 0;
  f.setPoll(async (accounts) => {
    polls += 1;
    if (polls !== 1) return false;
    accounts.splice(0, accounts.length, 'account-b');
    f.notifyGeneration();
    return true;
  });
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  assert.equal(polls, 2);
  assert.equal(f.calls(), 2, 'unchanged polls preserve the sticky decision');
  assert.deepEqual(f.appended.map(({ data }) => [data.action, data.reason]), [
    ['pin', 'initial'], ['release', 'credential-unavailable'], ['pin', 'credential-unavailable'],
  ]);
});

test('a failed external poll keeps the cached in-memory credential usable', async () => {
  const f = fixture();
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  f.setPoll(async () => { throw new Error('store temporarily unavailable'); });
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
  assert.equal(f.calls(), 1);
});

test('native resolution handles command and environment references on durable resume', async () => {
  const envName = 'HERDR_API_KEY_OBSERVER_TEST_REFERENCE';
  const previous = process.env[envName];
  process.env[envName] = 'resolved-env-secret';
  try {
    for (const expression of ['!printf command-secret', envName]) {
      const f = fixture({ entries: [pin('session-1')] });
      f.auth.credentials.list = () => [{ id: 7, credential: { type: 'api_key', key: expression } }];
      assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).apiKey, 'account-a');
      assert.equal(f.calls(), 1, 'the expression must go through native resolution');
      assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).apiKey, 'account-a');
      assert.equal(f.calls(), 1, 'the resolved live credential can still stay sticky');
    }
  } finally {
    if (previous === undefined) delete process.env[envName];
    else process.env[envName] = previous;
  }
});

test('branch activation persists its own pin and resumes without importing parent pins', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  f.setSessionId('branch-2');
  f.activate('session_branch');
  assert.equal(f.state.selections.size, 0, 'copied parent journal entries belong to the parent session');
  await f.auth.keys.getWithCredential('opencode-go', 'branch-2');
  assert.deepEqual(f.appended.map(({ data }) => [data.sessionId, data.credentialId]), [['session-1', 7], ['branch-2', 8]]);
  const resumed = fixture({ selected: ['account-a', 'account-b'], entries: f.appended });
  resumed.setSessionId('branch-2');
  resumed.activate('session_switch');
  assert.equal((await resumed.auth.keys.getWithCredential('opencode-go', 'branch-2')).credentialId, 8);
  assert.equal(resumed.calls(), 0);
  assert.equal(resumed.state.selections.size, 1);
  assert.deepEqual(resumed.appended, []);
});

test('shutdown clears only the owned scope and the last scope clears unmanaged secrets', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  const child = sessionScope(f.auth, 'child-a');
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await f.auth.keys.getWithCredential('opencode-go', 'child-a');
  await f.auth.keys.getWithCredential('opencode-go', 'unmanaged');
  child.shutdown();
  assert.equal(f.state.resolved.has('opencode-go\0child-a'), false);
  assert.equal(f.state.resolved.has('opencode-go\0session-1'), true);
  assert.equal(f.state.resolved.has('opencode-go\0unmanaged'), true);
  assert.deepEqual(child.appended.map(({ data }) => data.action), ['pin'], 'cleanup is not a durable release');
  f.shutdown();
  for (const cache of [f.state.selections, f.state.resolved, f.state.pendingReasons, f.state.inflight]) assert.equal(cache.size, 0);
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.equal(f.state.resolved.size, 0, 'inactive observer wrappers do not retain credentials');
  f.activate();
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
  assert.equal(f.appended.length, 1, 'reactivation restores the durable pin without a duplicate');
});

test('unmanaged session churn is bounded while live sessions retain their pins', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  for (let index = 0; index < 300; index += 1) {
    await f.auth.keys.getWithCredential('opencode-go', `worker-${index}`);
  }
  assert.ok(f.state.selections.size <= 129);
  assert.ok(f.state.resolved.size <= 129);
  for (let index = 300; index < 600; index += 1) {
    const id = `worker-${index}`;
    await f.auth.keys.getWithCredential('opencode-go', id);
    f.auth.sessions.release('opencode-go', id);
  }
  assert.ok(f.state.pendingReasons.size <= 128);
  assert.ok(f.state.sessionEpochs.size <= 129);
  const calls = f.calls();
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
  assert.equal(f.calls(), calls);
  assert.deepEqual(f.appended.map(({ data }) => data.action), ['pin']);
});

test('late selection after reset cannot replace the new pin', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  const pending = f.deferSelection();
  const stale = f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await pending.started;
  f.sessionManager.appendResetBoundary();
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  pending.resolve();
  assert.equal((await stale).credentialId, 7);
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  assert.deepEqual(f.appended.map(({ data }) => data.credentialId), [8]);
});

test('late selection after shutdown does not restore secrets or journal entries', async () => {
  const f = fixture();
  const pending = f.deferSelection();
  const stale = f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await pending.started;
  f.shutdown();
  pending.resolve();
  await stale;
  for (const cache of [f.state.selections, f.state.resolved, f.state.pendingReasons, f.state.inflight]) assert.equal(cache.size, 0);
  assert.deepEqual(f.appended, []);
});

test('a revived scope cancels its predecessor selection without losing its own pin', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  const pending = f.deferSelection();
  const stale = f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await pending.started;
  const revived = sessionScope(f.auth, 'session-1');
  f.shutdown();
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  pending.resolve();
  await stale;
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
  assert.deepEqual(f.appended, []);
  assert.deepEqual(revived.appended.map(({ data }) => data.credentialId), [8]);
});

test('credential generations prevent a pending old resolution from populating the cache', async () => {
  const f = fixture({ selected: ['account-a', 'account-b'] });
  const pending = f.deferSelection();
  const stale = f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await pending.started;
  f.notifyGeneration();
  pending.resolve();
  await stale;
  assert.equal(f.state.resolved.size, 0);
  assert.deepEqual(f.appended, []);
  assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 8);
});

test('reset during external polling cannot populate an obsolete selection', async () => {
  const f = fixture();
  let resolvePoll;
  let enteredPoll;
  const started = new Promise((resolve) => { enteredPoll = resolve; });
  const polling = new Promise((resolve) => { resolvePoll = resolve; });
  f.setPoll(async () => { enteredPoll(); await polling; return false; });
  const stale = f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await started;
  f.sessionManager.appendResetBoundary();
  resolvePoll();
  await stale;
  assert.equal(f.state.resolved.size, 0);
  assert.deepEqual(f.appended, []);
});

test('late usage-limit operations cannot release a revived scope pin', async () => {
  for (const operation of ['markReached', 'rotate']) {
    const f = fixture({ selected: ['account-a', 'account-b'] });
    await f.auth.keys.getWithCredential('opencode-go', 'session-1');
    const late = f.auth.limits[operation]('opencode-go', 'session-1');
    const revived = sessionScope(f.auth, 'session-1', f.appended);
    f.shutdown();
    await late;
    assert.equal((await f.auth.keys.getWithCredential('opencode-go', 'session-1')).credentialId, 7);
    assert.deepEqual(revived.appended, []);
  }
});

test('credential generation cannot mask a restriction added to an in-flight request', async () => {
  const f = fixture();
  const pending = f.deferSelection();
  const stale = f.auth.keys.getWithCredential('opencode-go', 'session-1');
  await pending.started;
  f.notifyGeneration();
  f.restrict('session-1');
  pending.resolve();
  await assert.rejects(stale, (error) => error === f.restrictionError);
  assert.equal(f.state.resolved.size, 0);
  assert.deepEqual(f.appended, []);
});
