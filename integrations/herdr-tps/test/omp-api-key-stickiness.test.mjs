import assert from 'node:assert/strict';
import test from 'node:test';
import {
  ompApiKeyStickyEntryType,
  registerOmpApiKeyStickiness,
} from '../lib/omp-api-key-stickiness.mjs';

function fixture({ entries = [], blocked = [] } = {}) {
  const handlers = new Map();
  const appended = [];
  const selected = ['account-a', 'account-b'];
  let calls = 0;
  const auth = {
    getApiKey: async () => selected[calls++ % selected.length],
    exportSnapshot: () => ({
      credentials: selected.map((key, index) => ({
        id: index + 7,
        provider: 'opencode-go',
        credential: { type: 'api_key', source: 'login', key },
      })),
    }),
    listCredentialBlocks: (ids) => blocked.filter((item) => ids.includes(item.credentialId)),
    releaseSessionCredentialForReselection: () => true,
    markUsageLimitReached: async () => ({ switched: true }),
    rotateSessionCredential: async () => true,
  };
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
    appendEntry: (customType, data) => appended.push({ type: 'custom', customType, data }),
  };
  registerOmpApiKeyStickiness(pi);
  handlers.get('session_start')({}, {
    modelRegistry: { authStorage: auth },
    sessionManager: { getEntries: () => entries },
  });
  return { auth, appended, blocked, calls: () => calls };
}

test('pins one opencode-go API key without emitting collection records', async () => {
  const f = fixture();
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-a');
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-a');
  assert.equal(f.calls(), 1);
  assert.deepEqual(f.appended, []);
});

test('restores an exact-session pin but rejects an inherited parent pin', async () => {
  const entries = [
    {
      type: 'custom',
      customType: ompApiKeyStickyEntryType,
      data: {
        action: 'pin',
        provider: 'opencode-go',
        sessionId: 'parent-session',
        credentialId: 8,
      },
    },
  ];
  const f = fixture({ entries });
  assert.equal(await f.auth.getApiKey('opencode-go', 'parent-session'), 'account-b');
  assert.equal(f.calls(), 0);
  assert.equal(await f.auth.getApiKey('opencode-go', 'child-session'), 'account-a');
  assert.equal(f.calls(), 1);
});

test('reselects after a block or an explicit release', async () => {
  const f = fixture();
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-a');
  f.blocked.push({ credentialId: 7, blockedUntilMs: Date.now() + 60_000 });
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-b');
  assert.equal(f.calls(), 2);

  f.blocked.length = 0;
  assert.equal(f.auth.releaseSessionCredentialForReselection('opencode-go', 'session-1'), true);
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-a');
  assert.equal(f.calls(), 3);
  assert.deepEqual(f.appended, []);
});

test('coalesces concurrent first selections and leaves other providers untouched', async () => {
  const f = fixture();
  const [first, second] = await Promise.all([
    f.auth.getApiKey('opencode-go', 'session-1'),
    f.auth.getApiKey('opencode-go', 'session-1'),
  ]);
  assert.deepEqual([first, second], ['account-a', 'account-a']);
  assert.equal(f.calls(), 1);

  assert.equal(await f.auth.getApiKey('another-provider', 'session-1'), 'account-b');
  assert.equal(await f.auth.getApiKey('another-provider', 'session-1'), 'account-a');
  assert.equal(f.calls(), 3);
});
