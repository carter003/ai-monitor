import test from 'node:test';
import assert from 'node:assert/strict';
import { CodexRolloutFollower } from '../lib/codex-rollout-follower.mjs';

test('partial UTF8 records preserve model and authoritative hidden output usage', () => {
  const models = [], usage = [];
  const follower = new CodexRolloutFollower({ onModel: m => models.push(m), onUsage: (...v) => usage.push(v) });
  const events = [
    { type: 'turn_context', payload: { model: '模型', collaboration_mode: { settings: { model: 'chosen' } } } },
    { type: 'event_msg', payload: { type: 'item_completed', item: { type: 'Reasoning', started_at_ms: 100, completed_at_ms: 2100 } } },
    { type: 'event_msg', payload: { type: 'item_completed', item: { type: 'AgentMessage', started_at_ms: 2100, completed_at_ms: 3100 } } },
    { type: 'token_usage_record', payload: { response_id: 'one', usage: { output_tokens: 600, reasoning_output_tokens: 500 } } },
    { type: 'token_usage_record', payload: { response_id: 'one', usage: { output_tokens: 600 } } },
    { type: 'event_msg', payload: { type: 'thread_settings_applied', thread_settings: { model: 'next' } } },
  ];
  const bytes = Buffer.from(events.map(e => JSON.stringify(e)).join('\n') + '\n');
  for (const byte of bytes) follower.append(Buffer.from([byte]));
  assert.deepEqual(models, ['chosen', 'next']);
  assert.deepEqual(usage, [[600, 3000]]);
  follower.append('{"type":"turn_context","payload":{"model":"partial"}}');
  assert.equal(models.length, 2);
  follower.append('\n'); assert.equal(models.at(-1), 'partial');
});
test('uses request interval then task duration without estimating text tokens', () => {
  const usage = [];
  const f = new CodexRolloutFollower({ onUsage: (...v) => usage.push(v) });
  f.consume({ type: 'turn_context', timestamp: '2026-01-01T00:00:00Z', payload: {} });
  f.consume({ type: 'token_usage_record', timestamp: '2026-01-01T00:00:02Z', payload: { usage: { output_tokens: 100 } } });
  assert.deepEqual(usage, [[100, 2000]]);
  const g = new CodexRolloutFollower({ onUsage: (...v) => usage.push(v) });
  g.consume({ type: 'token_usage_record', payload: { usage: { output_tokens: 20 } } });
  g.consume({ type: 'event_msg', payload: { type: 'task_complete', duration_ms: 1000 } });
  assert.deepEqual(usage.at(-1), [20, 1000]);
});
