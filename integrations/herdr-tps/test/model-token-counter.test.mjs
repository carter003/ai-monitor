import assert from 'node:assert/strict';
import test from 'node:test';
import { tokenCounterForModel } from '../lib/model-token-counter.mjs';

test('uses exact modern and legacy encodings for provider-qualified models', () => {
  assert.equal(tokenCounterForModel('codex/gpt-5.6-sol')('中文'), 1);
  assert.equal(tokenCounterForModel('openai/gpt-4')('中文'), 2);
});

test('uses byte estimation only for unknown models', () => {
  assert.equal(tokenCounterForModel('unknown-model')('中文'), 2);
  assert.throws(() => tokenCounterForModel('codex/gpt-5.6-sol')('<|endoftext|>'));
});
