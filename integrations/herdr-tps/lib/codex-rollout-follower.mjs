import { StringDecoder } from 'node:string_decoder';

// Owns no file handles. Consumers can feed arbitrary byte boundaries safely.
export class CodexRolloutFollower {
  constructor({ onModel = () => {}, onUsage = () => {} } = {}) {
    this.onModel = onModel;
    this.onUsage = onUsage;
    this.decoder = new StringDecoder('utf8');
    this.pending = '';
    this.startedAt = undefined;
    this.completedAt = undefined;
    this.requestAt = undefined;
    this.seenResponses = new Set();
  }

  append(chunk) {
    this.pending += Buffer.isBuffer(chunk) ? this.decoder.write(chunk) : chunk;
    let end;
    while ((end = this.pending.indexOf('\n')) >= 0) {
      const line = this.pending.slice(0, end);
      this.pending = this.pending.slice(end + 1);
      try { this.consume(JSON.parse(line)); } catch { /* Ignore malformed records, retain partial lines. */ }
    }
  }

  consume(record) {
    const p = record.payload ?? {};
    if (record.type === 'turn_context') {
      this.onModel(p.collaboration_mode?.settings?.model ?? p.model);
      this.requestAt = Date.parse(record.timestamp);
      this.startedAt = this.completedAt = undefined;
    }
    if (record.type === 'event_msg' && p.type === 'thread_settings_applied') {
      this.onModel(p.thread_settings?.collaboration_mode?.settings?.model ?? p.thread_settings?.model);
    }
    if (record.type === 'event_msg' && p.type === 'item_completed' &&
        ['Reasoning', 'AgentMessage'].includes(p.item?.type)) {
      const start = p.started_at_ms ?? p.item.started_at_ms;
      const end = p.completed_at_ms ?? p.item.completed_at_ms;
      if (Number.isFinite(start)) this.startedAt = Math.min(this.startedAt ?? start, start);
      if (Number.isFinite(end)) this.completedAt = Math.max(this.completedAt ?? end, end);
    }
    if (record.type === 'token_usage_record') {
      if (p.response_id && this.seenResponses.has(p.response_id)) return;
      if (p.response_id) {
        this.seenResponses.add(p.response_id);
        if (this.seenResponses.size > 1024) this.seenResponses.delete(this.seenResponses.values().next().value);
      }
      this.pendingUsage = undefined;
      const at = Date.parse(record.timestamp);
      const duration = this.completedAt > this.startedAt
        ? this.completedAt - this.startedAt : at - this.requestAt;
      const tokens = p.usage?.output_tokens;
      if (Number.isFinite(tokens) && tokens >= 0 && Number.isFinite(duration) && duration > 0) {
        this.onUsage(tokens, duration);
      } else if (Number.isFinite(tokens) && tokens >= 0) {
        this.pendingUsage = tokens;
      }
      this.requestAt = at;
      this.startedAt = this.completedAt = undefined;
    }
    if (record.type === 'event_msg' && p.type === 'task_complete' && this.pendingUsage !== undefined) {
      if (Number.isFinite(p.duration_ms) && p.duration_ms > 0) this.onUsage(this.pendingUsage, p.duration_ms);
      this.pendingUsage = undefined;
    }
  }
}
