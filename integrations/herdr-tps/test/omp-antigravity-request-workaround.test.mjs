import assert from 'node:assert/strict';
import test from 'node:test';
import { registerOmpAntigravityRequestWorkaround } from '../lib/omp-antigravity-request-workaround.mjs';

function captureHandler() {
  let handler;
  registerOmpAntigravityRequestWorkaround({
    on(event, candidate) {
      assert.equal(event, 'before_provider_request');
      handler = candidate;
    },
  });
  return handler;
}

test('changes only the blocked sentence in an Antigravity agent system instruction', () => {
  const handler = captureHandler();
  const contents = [{ role: 'user', parts: [{ text: 'RFC 2119 must remain in user input' }] }];
  const tools = [{ functionDeclarations: [{ name: 'RFC 2119 tool' }] }];
  const payload = {
    requestType: 'agent',
    userAgent: 'antigravity',
    request: {
      systemInstruction: {
        parts: [
          {
            text: 'prefix\nRFC 2119: MUST, REQUIRED, SHOULD, RECOMMENDED, MAY, OPTIONAL.\nsuffix',
          },
        ],
      },
      contents,
      tools,
    },
  };

  assert.equal(handler({ payload }), payload);
  assert.equal(
    payload.request.systemInstruction.parts[0].text,
    'prefix\nRFC 21\u200B19: MUST, REQUIRED, SHOULD, RECOMMENDED, MAY, OPTIONAL.\nsuffix',
  );
  assert.equal(payload.request.contents, contents);
  assert.equal(payload.request.tools, tools);
});

test('leaves non-Antigravity requests unchanged', () => {
  const handler = captureHandler();
  const text = 'RFC 2119: MUST, REQUIRED, SHOULD, RECOMMENDED, MAY, OPTIONAL.';
  const payload = {
    requestType: 'agent',
    userAgent: 'gemini-cli',
    request: { systemInstruction: { parts: [{ text }] } },
  };

  assert.equal(handler({ payload }), undefined);
  assert.equal(payload.request.systemInstruction.parts[0].text, text);
});
