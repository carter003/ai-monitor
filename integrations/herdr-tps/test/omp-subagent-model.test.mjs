import test from 'node:test';
import assert from 'node:assert/strict';
import extension, { registerOmpSubagentModelPolicy } from '../omp-subagent-model.js';
test('eval roles remain explicit and missing override registry is safe', () => {
  let handler;
  registerOmpSubagentModelPolicy({ on: (_, h) => { handler = h; } }, {});
  const context = { models: { current: () => ({ provider: 'provider', id: 'parent' }) } };
  assert.equal(handler({ modelRole: 'slow', invocationKind: 'eval' }, context), undefined);
  assert.equal(handler({ modelRole: 'slow', invocationKind: 'task' }, context).model, 'provider/parent');
});
test('unavailable internal imports warn once and leave native dispatch intact', async () => {
  const warnings = [];
  await extension({ logger: { warn: m => warnings.push(m) }, on() { assert.fail('No hooks without host registry'); } });
  assert.equal(warnings.length, 1);
});

function modelPolicyHarness() {
  const handlers = new Map();
  const settings = {
    overrides: {},
    roles: { default: 'kiro/claude-opus-5.5' },
    getModelRole(role) { return this.roles[role]; },
  };
  let model = { provider: 'opencode-go', id: 'hungerg-free' };
  registerOmpSubagentModelPolicy({
    pi: { settings },
    on: (event, handler) => handlers.set(event, handler),
  }, {
    agentModelOverrides: { get: (scope) => scope.overrides },
    getUpstreamPin: () => undefined,
  });
  return {
    settings,
    switchModel(next) { model = next; },
    route(event) {
      const result = handlers.get('before_subagent_spawn')(event, {
        models: { current: () => model },
      });
      return result?.model ?? event.patterns;
    },
  };
}

test('unconfigured specialist and worker roles inherit the live parent, not the saved default', () => {
  const policy = modelPolicyHarness();
  for (const invocationKind of ['task']) {
    for (const [agent, modelRole] of [
      ['reviewer', 'slow'], ['security-reviewer', 'slow'],
      ['scout', 'smol'], ['sonic', 'smol'], ['task', 'task'],
    ]) {
      assert.equal(policy.route({
        agent, invocationKind, modelRole, patterns: ['kiro/claude-opus-5.5'],
      }), 'opencode-go/hungerg-free');
    }
  }
  policy.switchModel({ provider: 'openai', id: 'gpt-6.1-sol' });
  assert.equal(policy.route({
    agent: 'reviewer', modelRole: 'slow', patterns: ['kiro/claude-opus-5.5'],
  }), 'openai/gpt-6.1-sol');
});

test('preserves explicit per-agent, role, literal and tagged model choices', () => {
  const policy = modelPolicyHarness();
  const patterns = ['kiro/claude-opus-5.5'];
  const event = { agent: 'reviewer', modelRole: 'slow', patterns };
  policy.settings.overrides.reviewer = '@slow';
  assert.deepEqual(policy.route(event), patterns);
  policy.settings.overrides.reviewer = ['@slow'];
  assert.deepEqual(policy.route(event), patterns);
  policy.settings.overrides.reviewer = [];
  assert.equal(policy.route(event), 'opencode-go/hungerg-free');
  delete policy.settings.overrides.reviewer;
  policy.settings.roles.slow = patterns[0];
  assert.deepEqual(policy.route(event), patterns);
  delete policy.settings.roles.slow;
  assert.deepEqual(policy.route({ agent: 'reviewer', patterns }), patterns);
  assert.deepEqual(policy.route({ agent: 'm1', patterns }), patterns);
});

test('reads updated role and agent assignments on each dispatch without changing sibling routes', () => {
  const policy = modelPolicyHarness();
  const event = { agent: 'reviewer', modelRole: 'slow', patterns: ['kiro/claude-opus-5.5'] };
  assert.equal(policy.route(event), 'opencode-go/hungerg-free');
  policy.settings.roles.slow = 'kiro/claude-opus-5.5';
  assert.deepEqual(policy.route(event), event.patterns);
  delete policy.settings.roles.slow;
  policy.settings.overrides.scout = 'kiro/claude-opus-5.5';
  assert.equal(policy.route(event), 'opencode-go/hungerg-free');
});


test('eval default roles inherit but explicit, unknown, user and configured roles remain intact', () => {
  let handler;
  const settings = { getModelRole: () => undefined };
  registerOmpSubagentModelPolicy({ pi: { settings }, on: (_, h) => { handler = h; } }, {
    agentDefinitions: [
      { name: 'reviewer', model: ['@slow'] }, { name: 'scout', model: ['@smol'] },
      { name: 'user-reviewer', model: '@slow' }, { name: 'm1' },
    ],
  });
  const context = { models: { current: () => ({ provider: 'p', id: 'parent' }) } };
  for (const [agent, modelRole] of [['reviewer','slow'], ['scout','smol'], ['user-reviewer','slow']]) {
    assert.equal(handler({ agent, modelRole, invocationKind: 'eval' }, context).model, 'p/parent');
  }
  for (const [agent, modelRole] of [['m1','slow'], ['reviewer','smol'], ['unknown','slow']]) {
    assert.equal(handler({ agent, modelRole, invocationKind: 'eval' }, context), undefined);
  }
  settings.getModelRole = () => 'p/explicit';
  assert.equal(handler({ agent:'reviewer', modelRole:'slow', invocationKind:'eval' }, context), undefined);
});
