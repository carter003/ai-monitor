import { Buffer } from 'node:buffer';
import { createRequire } from 'node:module';

// Use explicit CJS exports: compiled OMP Bun cannot resolve the dual-mode encoding exports.
const require = createRequire(import.meta.url);
let cl100kCounter;
let o200kCounter;

function countCl100kTokens(text) {
  cl100kCounter ??= require('gpt-tokenizer/cjs/encoding/cl100k_base').countTokens;
  return cl100kCounter(text);
}

function countO200kTokens(text) {
  o200kCounter ??= require('gpt-tokenizer/cjs/encoding/o200k_base').countTokens;
  return o200kCounter(text);
}

const MODERN_OPENAI_MODEL = /^(?:gpt-(?:4o|4\.1|5|oss)|o[134](?:-|$)|codex)/i;
const LEGACY_OPENAI_MODEL = /^gpt-(?:3\.5|4(?:-|$))/i;

function normalizedModelName(model) {
  return String(model ?? '')
    .trim()
    .split('/')
    .at(-1);
}

export function estimateFallbackTokens(text) {
  if (!text) {
    return 0;
  }
  return Math.ceil(Buffer.byteLength(text, 'utf8') / 4);
}

export function tokenCounterForModel(model) {
  const modelName = normalizedModelName(model);
  if (MODERN_OPENAI_MODEL.test(modelName)) {
    return countO200kTokens;
  }
  if (LEGACY_OPENAI_MODEL.test(modelName)) {
    return countCl100kTokens;
  }
  return estimateFallbackTokens;
}
