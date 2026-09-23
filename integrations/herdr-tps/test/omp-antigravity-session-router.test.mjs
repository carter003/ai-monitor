import assert from 'node:assert/strict';
import test from 'node:test';
import { registerOmpAntigravitySessionRouter } from '../lib/omp-antigravity-session-router.mjs';

function report(email, projectId, remainingFraction) {
  return {
    provider: 'google-antigravity',
    metadata: { email, projectId },
    limits: [{
      id: 'google-antigravity:google:default:gemini-5h',
      label: 'Gemini',
      scope: { windowId: '5h' },
      amount: { remainingFraction },
    }],
  };
}

function fixture({ remaining = [0.2, 0.1], existing = false, modelProvider = 'google-antigravity' } = {}) {
  const handlers = new Map();
  const appended = [];
  const pinned = [];
  const modelChanges = [];
  const thinking = [];
  const activeCredentials = new Map();
  let healthCalls = 0;
  let usageFetches = 0;
  const accounts = [
    { credentialId: 7, email: 'a@example.test', projectId: 'project-a' },
    { credentialId: 11, email: 'b@example.test', projectId: 'project-b' },
  ];
  const auth = {
    listOAuthAccounts: (_provider, session) => accounts.map((account) => ({
      ...account,
      active: activeCredentials.get(session) === account.credentialId,
    })),
    listCredentialBlocks: () => [],
    fetchUsageReports: async () => {
      usageFetches += 1;
      return [
        report(accounts[0].email, accounts[0].projectId, remaining[0]),
        report(accounts[1].email, accounts[1].projectId, remaining[1]),
      ];
    },
    pinSessionOAuthAccount: (...args) => {
      pinned.push(args);
      activeCredentials.set(args[1], args[2]);
      return true;
    },
    getModelUsageHealth: async () => {
      healthCalls += 1;
      return { state: 'reserve', accounts: [] };
    },
  };
  const context = {
    model: { provider: modelProvider, id: 'gemini-3.8-flash' },
    modelRegistry: {
      authStorage: auth,
      find: (provider, id) => ({ provider, id }),
    },
    sessionManager: {
      getSessionId: () => 'session-1',
      getEntries: () => existing
        ? [{ type: 'message', message: { role: 'assistant', content: [] } }]
        : [],
    },
  };
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
    appendEntry: (customType, data) => appended.push({ customType, data }),
    setModel: async (model) => {
      modelChanges.push(model);
      context.model = model;
      return true;
    },
    setThinkingLevel: (level) => thinking.push(level),
  };
  registerOmpAntigravitySessionRouter(pi);
  const started = handlers.get('session_start')({}, context);
  return {
    auth,
    handlers,
    context,
    appended,
    pinned,
    modelChanges,
    thinking,
    started,
    usageFetches: () => usageFetches,
    healthCalls: () => healthCalls,
  };
}

test('pins the Antigravity account with the most Gemini 5h quota once', async () => {
  const f = fixture({ remaining: [0.2, 0.4] });
  await f.handlers.get('before_agent_start')({}, f.context);
  assert.deepEqual(f.pinned, [['google-antigravity', 'session-1', 11]]);
  assert.equal(f.appended[0].data.action, 'pin-antigravity');
  assert.equal(f.appended[0].data.remainingFraction, 0.4);
  assert.deepEqual(f.modelChanges, []);

  assert.deepEqual(
    await f.auth.getModelUsageHealth('google-antigravity', { sessionId: 'session-1' }),
    { state: 'healthy', accounts: [] },
  );
  assert.equal(f.healthCalls(), 0);
});
test('preselects the highest-quota account before title generation starts', async () => {
  const f = fixture({ remaining: [0.2, 0.4] });
  await f.started;

  const active = f.auth
    .listOAuthAccounts('google-antigravity', 'session-1')
    .find((account) => account.active);
  assert.equal(active?.credentialId, 11);

  f.context.model = { provider: 'google-antigravity', id: 'gemini-3.8-flash' };
  await f.handlers.get('before_agent_start')({}, f.context);
  assert.deepEqual(f.pinned, [['google-antigravity', 'session-1', 11]]);
  assert.equal(f.appended[0].data.action, 'pin-antigravity');
});

test('falls back a new session when every usable Antigravity account is below 15 percent', async () => {
  const f = fixture({ remaining: [0.14, 0.09] });
  await f.handlers.get('before_agent_start')({}, f.context);
  assert.deepEqual(f.pinned, [['google-antigravity', 'session-1', 7]]);
  assert.deepEqual(f.modelChanges, [{ provider: 'opencode-go', id: 'deepseek-v4.1-flash' }]);
  assert.deepEqual(f.thinking, ['high']);
  assert.equal(f.appended[0].data.action, 'fallback');
  assert.equal(f.appended[0].data.remainingFraction, 0.14);
});

test('keeps Antigravity at exactly 15 percent and fails open when any account usage is unknown', async () => {
  const boundary = fixture({ remaining: [0.15, 0.1] });
  await boundary.handlers.get('before_agent_start')({}, boundary.context);
  assert.deepEqual(boundary.modelChanges, []);
  assert.equal(boundary.appended[0].data.action, 'pin-antigravity');
  assert.equal(boundary.appended[0].data.remainingFraction, 0.15);

  const unknown = fixture({ remaining: [0.14, undefined] });
  await unknown.handlers.get('before_agent_start')({}, unknown.context);
  assert.deepEqual(unknown.modelChanges, []);
  assert.equal(unknown.appended[0].data.action, 'keep-antigravity');
  assert.equal(unknown.appended[0].data.reason, 'usage-unknown');
});

test('never reroutes an existing conversation and suppresses its proactive preflight', async () => {
  const f = fixture({ remaining: [0.01, 0.02], existing: true });
  await f.handlers.get('before_agent_start')({}, f.context);
  assert.deepEqual(f.pinned, []);
  assert.deepEqual(f.modelChanges, []);
  assert.deepEqual(
    await f.auth.getModelUsageHealth('google-antigravity', { sessionId: 'session-1' }),
    { state: 'healthy', accounts: [] },
  );
  assert.equal(f.healthCalls(), 0);
});

test('does not preselect Antigravity for a non-Antigravity startup', async () => {
  const f = fixture({ modelProvider: 'openai-codex' });
  await f.started;
  assert.equal(f.usageFetches(), 0);
  assert.deepEqual(f.pinned, []);

  f.context.model = { provider: 'google-antigravity', id: 'gemini-3.8-flash' };
  await f.handlers.get('before_agent_start')({}, f.context);
  assert.equal(f.usageFetches(), 1);
  assert.deepEqual(f.pinned, [['google-antigravity', 'session-1', 7]]);
  assert.deepEqual(f.modelChanges, []);
  assert.deepEqual(
    await f.auth.getModelUsageHealth('openai-codex', { sessionId: 'session-1' }),
    { state: 'healthy', accounts: [] },
  );
});
