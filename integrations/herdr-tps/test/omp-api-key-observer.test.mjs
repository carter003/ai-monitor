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
    keys: {
      getWithCredential: async () => {
        const apiKey = selected[Math.min(calls++, selected.length - 1)];
        return { apiKey, credentialId: apiKey === 'account-a' ? 7 : 8 };
      },
    },
    sessions: { release: () => true },
    limits: {
      markReached: async () => ({ switched: true }),
      rotate: async () => true,
    },
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
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), {
    apiKey: 'account-a',
    credentialId: 7,
  });
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), {
    apiKey: 'account-a',
    credentialId: 7,
  });
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), {
    apiKey: 'account-b',
    credentialId: 8,
  });

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
  await f.auth.keys.getWithCredential('opencode-go', 'session-1');
  assert.equal(f.auth.sessions.release('opencode-go', 'session-1'), true);
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
  assert.deepEqual(await f.auth.keys.getWithCredential('opencode-go', 'session-1'), {
    apiKey: 'account-a',
    credentialId: 7,
  });
  assert.deepEqual(f.appended, []);
});
