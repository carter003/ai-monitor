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
  const auth = {
    getApiKey: async () => selected[Math.min(calls++, selected.length - 1)],
    exportSnapshot: () => ({
      credentials: ['account-a', 'account-b'].map((key, index) => ({
        id: index + 7,
        provider: 'opencode-go',
        credential: { type: 'api_key', source: 'login', key },
      })),
    }),
    releaseSessionCredentialForReselection: () => true,
    markUsageLimitReached: async () => ({ switched: true }),
    rotateSessionCredential: async () => true,
  };
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
    appendEntry: (customType, data) => appended.push({ type: 'custom', customType, data }),
  };
  registerOmpApiKeyObserver(pi);
  handlers.get('session_start')({}, {
    modelRegistry: { authStorage: auth },
    sessionManager: { getEntries: () => entries },
  });
  return { auth, appended, calls: () => calls };
}

test('observes every selection but records only account changes', async () => {
  const f = fixture();
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-a');
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-a');
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-b');

  assert.equal(f.calls(), 3, 'the observer must never pin or bypass OMP selection');
  assert.deepEqual(
    f.appended.map((entry) => [entry.customType, entry.data.action, entry.data.credentialId]),
    [
      [ompApiKeySelectionEntryType, 'pin', 7],
      [ompApiKeySelectionEntryType, 'pin', 8],
    ],
  );
  assert.equal(JSON.stringify(f.appended).includes('account-a'), false);
  assert.equal(JSON.stringify(f.appended).includes('account-b'), false);
});

test('records release boundaries without changing OMP behavior', async () => {
  const f = fixture();
  await f.auth.getApiKey('opencode-go', 'session-1');
  assert.equal(f.auth.releaseSessionCredentialForReselection('opencode-go', 'session-1'), true);
  assert.deepEqual(f.appended.map((entry) => entry.data.action), ['pin', 'release']);
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
  assert.equal(await f.auth.getApiKey('opencode-go', 'session-1'), 'account-a');
  assert.deepEqual(f.appended, []);
});
