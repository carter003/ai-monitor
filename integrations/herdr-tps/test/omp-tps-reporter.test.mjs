import assert from 'node:assert/strict';
import test from 'node:test';
import { OmpTpsReporter } from '../lib/omp-tps-reporter.mjs';
import { registerOmpTpsHandlers } from '../omp-extension.mjs';

class Tokenizer {
  constructor(model) { this.model = model; }
  countTokens(text) { return text.length; }
}
function fixture() {
  const rates = [];
  const publisher = {
    resetForAgent: async () => {}, publishModel: async () => {},
    publishRate: async (rate) => rates.push(rate), clearModel: async () => {},
    clearAll: async () => {},
  };
  return { rates, reporter: new OmpTpsReporter({ publisher, Tokenizer, autoStart: false }) };
}

test('OMP matches upstream working-row trace including decimal precision and usage correction', async () => {
  const { reporter } = fixture();
  const samples = [];
  reporter.start('one', 'model', 0);
  for (let t = 0; t <= 8000; t += 250) {
    reporter.append('one', 'text', 'x'.repeat(25), 'model', t);
    if ([4000, 6000, 8000].includes(t)) {
      reporter.tick(t); samples.push(reporter.lastRate);
    }
  }
  reporter.pause(8000, 8000, 1250); samples.push(reporter.lastRate);
  reporter.tick(20000); samples.push(reporter.lastRate);
  reporter.start('two', 'model', 30000);
  for (let t = 30000; t <= 36000; t += 250) {
    reporter.append('two', 'toolcall', 'x'.repeat(15), 'model', t);
    if ([30000, 34000, 36000].includes(t)) {
      reporter.tick(t); samples.push(reporter.lastRate);
    }
  }
  reporter.pause(36000, 6000, 800); samples.push(reporter.lastRate);
  // Generated independently with OMP 18.2.5 src/utils/token-rate.ts.
  assert.deepEqual(samples, [0, 105.5, 104.5, 157.6, 157.6, 159.9, 129.9, 121.8, 145.6]);
  await reporter.close();
});

test('OMP does not inflate short completions past its evidence gate', async () => {
  const { reporter } = fixture();
  reporter.start('one', 'model', 0);
  reporter.append('one', 'text', 'short', 'model', 100);
  reporter.pause(200, 200, 10);
  assert.equal(reporter.lastRate, 0);
  reporter.seed(10734, 100000);
  assert.equal(reporter.lastRate, 107.3);
  reporter.resetSession(1000);
  reporter.tick(20000);
  assert.equal(reporter.lastRate, 0);
  await reporter.close();
});

test('OMP feeds tool arguments but not tool results or duplicate snapshots to its native tokenizer', () => {
  const handlers = new Map();
  const appended = [];
  const models = [];
  registerOmpTpsHandlers({ on: (event, fn) => handlers.set(event, fn) }, {
    usesOmpNativeSemantics: true,
    configureTokenModel: (model) => models.push(model),
    start() {}, append: (...args) => appended.push(args), pause() {},
  });
  const model = { id: 'deepseek-v4.1-flash', tokenizer: 'deepseek-v3' };
  const message = { role: 'assistant', timestamp: 1000, model: model.id };
  const context = { model };
  handlers.get('message_start')({ message }, context);
  for (const [type, delta] of [
    ['thinking_delta', 'think'], ['text_delta', 'answer'], ['toolcall_delta', '{"path":"a"}'],
    ['text_end', 'answer'],
  ]) handlers.get('message_update')({ message, assistantMessageEvent: { type, delta, content: delta, contentIndex: 0 } }, context);
  handlers.get('message_update')({ message: { role: 'toolResult' }, assistantMessageEvent: { type: 'text_delta', delta: 'result' } }, context);
  handlers.get('message_end')({ message: { ...message, content: [{ type: 'text', text: 'answer' }] } }, context);
  assert.deepEqual(appended.map(args => args[2]), ['think', 'answer', '{"path":"a"}']);
  assert.deepEqual(models, [model]);
});
