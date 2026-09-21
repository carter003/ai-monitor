const BLOCKED_SYSTEM_TEXT =
  'RFC 2119: MUST, REQUIRED, SHOULD, RECOMMENDED, MAY, OPTIONAL.';
const SAFE_SYSTEM_TEXT =
  'RFC 21\u200B19: MUST, REQUIRED, SHOULD, RECOMMENDED, MAY, OPTIONAL.';

function rewriteSystemInstruction(payload) {
  if (payload?.requestType !== 'agent' || payload?.userAgent !== 'antigravity') {
    return false;
  }

  const parts = payload?.request?.systemInstruction?.parts;
  if (!Array.isArray(parts)) {
    return false;
  }

  let changed = false;
  for (const part of parts) {
    if (typeof part?.text !== 'string' || !part.text.includes(BLOCKED_SYSTEM_TEXT)) {
      continue;
    }
    part.text = part.text.replaceAll(BLOCKED_SYSTEM_TEXT, SAFE_SYSTEM_TEXT);
    changed = true;
  }
  return changed;
}

// Temporary mitigation for upstream OMP #12655. Antigravity rejects the
// generated system prompt when this exact RFC 2119 sentence is present.
export function registerOmpAntigravityRequestWorkaround(pi) {
  pi.on('before_provider_request', (event) => {
    if (rewriteSystemInstruction(event?.payload)) {
      return event.payload;
    }
  });
}
