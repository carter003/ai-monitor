#!/usr/bin/env node
// Minimal local patch for omp (bun SEA binary). Full analysis: BALANCE-PATCH.md.
//
// User-confirmed routing rules (2026-09-13):
//   1. A NEW session uses the OAuth account with the MOST 5h remaining
//      ("谁的 5H 多,下一个新建会话就用谁").
//   2. When every account is down to 15% remaining, a NEW session uses OMP's
//      usage-aware preflight to route directly through retry.fallbackChains to
//      opencode-go/deepseek-v4.1-flash:high (config.yml enables the policy).
//      Existing transcripts skip that preflight and retain their sticky model.
//   3. Session stickiness stays untouched: an established session keeps its
//      pinned account and model; only the initial pick follows the rules.
//
// Four sites, all strictly LENGTH-PRESERVING:
//
// Site A -- auth-storage `#computeWindowRequiredDrain`:
//   Antigravity's Site-B-only marker uses pure remaining headroom. Every other
//   usage-ranked provider keeps upstream's headroom / remaining-hours score.
//
// Site B -- Antigravity strategy `findWindowLimits` span. The provider's
//   semantic id anchors the search; the nearest supported strategy body ends
//   at `healableBlockScopes`. Its minified helper symbols are extracted
//   from the complete shape, then re-emitted (whitespace-compressed, padded to
//   the exact original length) so that `primary` is the model family's 5h
//   window instead of "the window with the LEAST remaining".
//
// Site C -- usage-aware preflight (empty-transcript guard):
//   Settings-first short-circuit, then `agent.state.messages.length`.
//   Needle must occur exactly once.
//
// Site D -- API-key selector (session credential guard):
//   Reuses an existing, matching, unblocked API-key session credential before
//   usage ranking, but only when the pin was written after this UUIDv7 session
//   was created. A pin inherited or restored from an older session must not
//   suppress the new session's first usage ranking.

import { constants } from 'node:fs';
import { access, copyFile, open, readFile, rename, unlink } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const SITE_A_VARIANTS = [
  {
    version: '18.2.1',
    fn: 'lt',
    start: '  #lt(e, t, s) {\n',
    endAnchor: '  #Vt(e, t, s) {',
    it: 'ct',
    gt: 'wt',
  },
  {
    version: '18.2',
    fn: 'mt',
    start: '  #mt(e, t, s) {\n',
    endAnchor: '  #dt(e, t, s) {',
    it: 'yt',
    gt: 'lt',
  },
  {
    version: '18.1',
    fn: 'Ot',
    start: '  #Ot(e, t, s) {\n',
    endAnchor: '  #qt(e, t, s) {',
    it: 'it',
    gt: 'gt',
  },
];

const SITE_B_PROVIDER_ANCHOR = '    id: "google-antigravity",\n    fetchUsage:';
const SITE_B_END_ANCHOR = '    healableBlockScopes(e) {';
const SITE_C_VARIANTS = ['#X', '#z', '#B'];
const SITE_C_BODY =
  '    if (!this.#e.settings.get("retry.usageAwareFallback"))\n' +
  '      return false;\n' +
  '    const s = this.#e.model();\n' +
  '    if (!s)\n' +
  '      return false;\n' +
  '    const o = ';
const SITE_C_END = '(s, this.#e.thinkingLevel());';

const SITE_D_VARIANTS = [
  {
    version: '18.2.1',
    fn: '#he',
    start: '  async#he(e, t, s, o) {\n',
    endAnchor: '  #Te(e) {',
    sticky: '#te',
  },
  {
    version: '18.2',
    fn: '#Q',
    start: '  async#Q(e, t, s, o) {\n',
    endAnchor: '  #ve(e) {',
    sticky: '#oe',
  },
  {
    version: '18.1',
    fn: '#ne',
    start: '  async#ne(e, t, s, o) {\n',
    endAnchor: '  #Ce(e) {',
    sticky: '#Q',
  },
];

function usage() {
  return `Usage: patch-omp-antigravity-balance.mjs [--check] [path-to-omp]

  --check   Report patch state; never writes.

The target is required unless OMP_RUNTIME_BIN points to the managed OMP binary.
`;
}

function parseArgs(argv) {
  let check = false;
  let target;
  for (const arg of argv) {
    if (arg === '--check') {
      check = true;
    } else if (arg === '-h' || arg === '--help') {
      return { help: true };
    } else if (arg.startsWith('-')) {
      throw new Error(`unknown flag: ${arg}`);
    } else if (target) {
      throw new Error(`unexpected extra argument: ${arg}`);
    } else {
      target = arg;
    }
  }
  return { check, target };
}

function countOccurrences(buffer, needle) {
  let count = 0;
  let from = 0;
  while (from <= buffer.length - needle.length) {
    const at = buffer.indexOf(needle, from);
    if (at === -1) {
      return count;
    }
    count += 1;
    from = at + needle.length;
  }
  return count;
}

function padReplacement(coreText, spanLength, label) {
  const core = Buffer.from(coreText, 'latin1');
  const pad = spanLength - core.length;
  if (pad === 0) return core;
  if (pad < 4) {
    throw new Error(
      `internal: ${label} replacement (${core.length}) does not fit span (${spanLength})`,
    );
  }
  return Buffer.concat([
    core.subarray(0, core.length - 1),
    Buffer.from(` //${' '.repeat(pad - 3)}\n`, 'latin1'),
  ]);
}

function buildSiteAOriginal(desc) {
  return (
    `  #${desc.fn}(e, t, s) {\n` +
    `    const o = 1 - this.#${desc.it}(e);\n` +
    '    if (o <= 0)\n' +
    '      return 0;\n' +
    `    const n = this.#${desc.gt}(e?.window);\n` +
    '    const r = e?.window?.durationMs ?? s;\n' +
    '    let i = n === undefined ? r : n - t;\n' +
    '    if (Number.isFinite(r) && r > 0) {\n' +
    '      i = Math.min(i, r);\n' +
    '    }\n' +
    '    const a = Math.max(i, 60000) / (60 * 60 * 1000);\n' +
    '    return o / a;\n' +
    '  }\n'
  );
}

function buildSiteAReplacement(span, desc) {
  return padReplacement(
    `#${desc.fn}(e,t,s){\n` +
      `const o=1-this.#${desc.it}(e);if(o<=0)return 0;if(e?.id==="ag5h")return o;\n` +
      `const n=this.#${desc.gt}(e?.window),r=e?.window?.durationMs??s;let i=n===undefined?r:n-t;\n` +
      'if(Number.isFinite(r)&&r>0)i=Math.min(i,r);\n' +
      'const a=Math.max(i,60000)/(60*60*1000);return o/a;\n' +
      '}\n',
    span.length,
    'site A',
  );
}

function buildSiteBOriginal(symbols) {
  return (
    '    findWindowLimits(e, t) {\n' +
    `      return { primary: ${symbols.selector}(e, t)[0] };\n` +
    '    },\n' +
    `    scopeLimits: ${symbols.limits},\n` +
    '    blockScope(e) {\n' +
    `      const t = ${symbols.scope}(e?.modelId);\n` +
    '      return `counter:$' +
    '{t ?? "unknown"}`;\n' +
    '    },\n'
  );
}

function extractSiteBSymbols(span, patched = false) {
  const text = span.toString('latin1');
  const selector = patched
    ? /(?:let w=|const s=|return\{primary:)([$\w]+)\(e,t\)/.exec(text)?.[1]
    : /return \{ primary: ([$\w]+)\(e, t\)\[0\] \};/.exec(text)?.[1];
  const limits = (patched ? /scopeLimits:([$\w]+)[,;]/ : /scopeLimits: ([$\w]+),/).exec(text)?.[1];
  const scope = patched
    ? /return`counter:\$\{([$\w]+)\(e\?\.modelId\)/.exec(text)?.[1] ??
      /const t=([$\w]+)\(e\?\.modelId\);/.exec(text)?.[1]
    : /const t = ([$\w]+)\(e\?\.modelId\);/.exec(text)?.[1];
  return selector && limits && scope ? { selector, limits, scope } : undefined;
}

function buildSiteBReplacement(span, symbols) {
  return padReplacement(
    'findWindowLimits(e,t){\n' +
      `const s=${symbols.selector}(e,t),w=s.find(w=>w.id.endsWith("5h"))??s[0];\n` +
      'return{primary:w&&{...w,id:"ag5h"}}},\n' +
      `scopeLimits:${symbols.limits},blockScope(e){return\`counter:\${${symbols.scope}(e?.modelId)??"unknown"}\`},\n`,
    span.length,
    'site B',
  );
}

function buildSiteBLegacyReplacement(span, symbols) {
  return padReplacement(
    'findWindowLimits(e,t){\n' +
      `return{primary:${symbols.selector}(e,t).find(w=>w.id.endsWith("5h"))??${symbols.selector}(e,t)[0]};\n` +
      '},\n' +
      `scopeLimits:${symbols.limits},\n` +
      'blockScope(e){\n' +
      `const t=${symbols.scope}(e?.modelId);\n` +
      'return`counter:$' +
      '{t??"unknown"}`;\n' +
      '},\n',
    span.length,
    'legacy site B',
  );
}

function buildSiteCReplacement(fn, helper, settingsFirst = true) {
  const guard = settingsFirst
    ? '!this.#e.settings.get("retry.usageAwareFallback")||this.#e.agent.state.messages.length'
    : 'this.#e.agent.state.messages.length||!this.#e.settings.get("retry.usageAwareFallback")';
  return (
    `  async${fn}(e,t){\n` +
    `if(${guard})return false;\n` +
    'const s=this.#e.model();\n' +
    'if(!s)return false;\n' +
    `const o=${helper}(s,this.#e.thinkingLevel());  `
  );
}

function buildSiteDOriginal(desc, symbols) {
  // 18.2.1 renamed the minified iterators (d -> f) and the result binding (f -> d);
  // the shape is otherwise identical to 18.2/18.1.
  const iter = desc.version === '18.2.1' ? 'f' : 'd';
  const result = desc.version === '18.2.1' ? 'd' : 'f';
  return (
    `  async${desc.fn}(e, t, s, o) {\n` +
    `    const n = this.${symbols.credentials}(e).map((${iter}, g) => ({ credential: ${iter}, index: g })).filter((${iter}) => {\n` +
    `      if (${iter}.credential.type !== "api_key")\n` +
    '        return false;\n' +
    `      return o?.(${iter}.credential) ?? true;\n` +
    '    });\n' +
    '    if (n.length === 0)\n' +
    '      return;\n' +
    '    if (n.length === 1)\n' +
    '      return n[0];\n' +
    `    const r = this.${symbols.key}(e, "api_key");\n` +
    `    const i = this.${symbols.order}(r, t, n.length);\n` +
    '    const a = n[i[0]];\n' +
    '    const l = this.#g?.(e);\n' +
    '    if (!l) {\n' +
    `      for (const ${iter} of i) {\n` +
    `        const g = n[${iter}];\n` +
    `        if (!this.${symbols.blocked}(e, r, g.index)) {\n` +
    '          return g;\n' +
    '        }\n' +
    '      }\n' +
    '      return a;\n' +
    '    }\n' +
    '    const u = {\n' +
    '      modelId: s?.modelId\n' +
    '    };\n' +
    '    const p = l.blockScope?.(u);\n' +
    `    const c = ${symbols.scopes}(e, l, u, p);\n` +
    `    const ${result} = await this.${symbols.he}({\n` +
    '      providerKey: r,\n' +
    '      provider: e,\n' +
    '      order: i,\n' +
    '      credentials: n,\n' +
    '      options: s,\n' +
    '      strategy: l,\n' +
    '      rankingContext: u,\n' +
    '      blockScope: p,\n' +
    '      blockScopes: c\n' +
    '    });\n' +
    `    return ${result}[0]?.selection ?? a;\n` +
    '  }\n'
  );
}
function extractSiteDSymbols(span, patched = false) {
  const text = span.toString('latin1');
  const credentials = /this\.(#[$\w]+)\(e\)\.map/.exec(text)?.[1];
  const key = (
    patched ? /const r=this\.(#[$\w]+)\(e,/ : /const r = this\.(#[$\w]+)\(e, "api_key"\);/
  ).exec(text)?.[1];
  const order = (
    patched ? /i=this\.(#[$\w]+)\(r,t,n\.length\)/ : /const i = this\.(#[$\w]+)\(r, t, n\.length\);/
  ).exec(text)?.[1];
  const blocked = (
    patched ? /!this\.(#[$\w]+)\(e,r,f\.index/ : /if \(!this\.(#[$\w]+)\(e, r, g\.index\)\)/
  ).exec(text)?.[1];
  const scopes = (
    patched ? /c=l\?([$\w]+)\(e,l,u,p\):undefined;/ : /const c = ([$\w]+)\(e, l, u, p\);/
  ).exec(text)?.[1];
  const he = (
    patched ? /const \w+=await this\.(#[$\w]+)\(\{/ : /const \w+ = await this\.(#[$\w]+)\(\{/
  ).exec(text)?.[1];

  return credentials && key && order && blocked && scopes && he
    ? { credentials, key, order, blocked, scopes, he }
    : undefined;
}

function buildSiteDReplacement(span, desc, symbols) {
  return padReplacement(
    `async${desc.fn}(e,t,s,o){\n` +
      `const n=this.${symbols.credentials}(e).map((f,g)=>({credential:f,index:g})).filter(f=>f.credential.type==="api_key"&&(o?.(f.credential)??true));\n` +
      `if(!n.length)return;const r=this.${symbols.key}(e,"api_key"),i=this.${symbols.order}(r,t,n.length),a=n[i[0]],l=this.#g?.(e),u={modelId:s?.modelId},p=l?.blockScope?.(u),c=l?${symbols.scopes}(e,l,u,p):undefined;\n` +
      `const h=this.${desc.sticky}(e,t),v=parseInt(t?.replaceAll("-","").slice(0,12),16);if(h?.type==="api_key"&&(!Number.isFinite(v)||h.lastUsedAtMs>=v)){const f=n.find(g=>g.index===h.index);if(f&&!this.${symbols.blocked}(e,r,f.index,c))return f}\n` +
      `if(n.length===1)return n[0];if(!l){for(const f of i){const g=n[f];if(!this.${symbols.blocked}(e,r,g.index))return g}return a}\n` +
      `const d=await this.${symbols.he}({providerKey:r,provider:e,order:i,credentials:n,options:s,strategy:l,rankingContext:u,blockScope:p,blockScopes:c});\n` +
      'return d[0]?.selection??a}\n',
    span.length,
    'site D',
  );
}

// Site D as emitted by the first sticky patch. Treat it as an upgradeable
// legacy state: it trusted every matching pin, including one inherited from an
// older session before the new session had made its first request.
function buildSiteDLegacyReplacement(span, desc, symbols) {
  return padReplacement(
    `async${desc.fn}(e,t,s,o){\n` +
      `const n=this.${symbols.credentials}(e).map((f,g)=>({credential:f,index:g})).filter(f=>f.credential.type==="api_key"&&(o?.(f.credential)??true));\n` +
      `if(!n.length)return;const r=this.${symbols.key}(e,"api_key"),i=this.${symbols.order}(r,t,n.length),a=n[i[0]],l=this.#g?.(e),u={modelId:s?.modelId},p=l?.blockScope?.(u),c=l?${symbols.scopes}(e,l,u,p):undefined;\n` +
      `const h=this.${desc.sticky}(e,t);if(h?.type==="api_key"){const f=n.find(g=>g.index===h.index);if(f&&!this.${symbols.blocked}(e,r,f.index,c))return f}\n` +
      `if(n.length===1)return n[0];if(!l){for(const f of i){const g=n[f];if(!this.${symbols.blocked}(e,r,g.index))return g}return a}\n` +
      `const d=await this.${symbols.he}({providerKey:r,provider:e,order:i,credentials:n,options:s,strategy:l,rankingContext:u,blockScope:p,blockScopes:c});\n` +
      'return d[0]?.selection??a}\n',
    span.length,
    'legacy site D',
  );
}

function inspect(buffer) {
  const problems = [];
  let siteA = 'unpatched';
  let siteASpan;
  let siteADesc;
  let siteB = 'unpatched';
  let siteBSpan;
  let siteBSymbols;
  let siteC = 'unpatched';
  let siteCOffset;
  let siteCReplacementText;
  let siteD = 'unpatched';
  let siteDSpan;
  let siteDDesc;
  let siteDSymbols;

  // Site A
  for (const desc of SITE_A_VARIANTS) {
    const needles = [desc.start, `#${desc.fn}(e,t,s){\n`];
    let found = false;
    for (const needle of needles) {
      for (let at = buffer.indexOf(needle); at !== -1; at = buffer.indexOf(needle, at + 1)) {
        const end = buffer.indexOf(desc.endAnchor, at);
        if (end !== -1 && end - at < 600) {
          siteADesc = desc;
          siteASpan = { start: at, end };
          found = true;
          break;
        }
      }
      if (found) break;
    }

    if (found) {
      const span = buffer.subarray(siteASpan.start, siteASpan.end);
      const expected = Buffer.from(buildSiteAOriginal(desc), 'latin1');
      const legacy = Buffer.from(buildSiteAOriginal(desc).replace('return o / a;', 'return o/**/;'), 'latin1');
      const replacement = buildSiteAReplacement(span, desc);

      if (Buffer.compare(span, expected) === 0) {
        siteA = 'unpatched';
      } else if (Buffer.compare(span, legacy) === 0) {
        siteA = 'unpatched'; // needs update to scoped ag5h
      } else if (Buffer.compare(span, replacement) === 0) {
        siteA = 'patched';
      } else {
        problems.push(
          `site A: required-drain span is ${span.length} bytes but is not a supported state for ${desc.version}`,
        );
      }
      break;
    }
  }
  if (!siteADesc) {
    problems.push('site A: required-drain function span not found; upstream drift, needs review');
  }

  // Site B
  const providerCount = countOccurrences(buffer, SITE_B_PROVIDER_ANCHOR);
  const providerAt = buffer.indexOf(SITE_B_PROVIDER_ANCHOR);
  const bOriginalStart = buffer.indexOf('    findWindowLimits(e, t) {\n', providerAt);
  const bPatchedStart = buffer.indexOf('findWindowLimits(e,t){\n', providerAt);
  const bStart =
    [bOriginalStart, bPatchedStart]
      .filter((offset) => offset !== -1)
      .sort((left, right) => left - right)[0] ?? -1;
  const bEnd = bStart === -1 ? -1 : buffer.indexOf(SITE_B_END_ANCHOR, bStart);
  if (
    providerCount !== 1 ||
    providerAt === -1 ||
    bStart === -1 ||
    bEnd === -1 ||
    bStart - providerAt > 2048 ||
    bEnd - bStart > 4096
  ) {
    problems.push(
      'site B: unique Antigravity strategy span not found; upstream drift, needs review',
    );
  } else {
    siteBSpan = { start: bStart, end: bEnd };
    const span = buffer.subarray(bStart, bEnd);
    const patchedShape = bPatchedStart === bStart;
    siteBSymbols = extractSiteBSymbols(span, patchedShape);
    if (!siteBSymbols) {
      problems.push('site B: could not extract current minified strategy symbols');
    } else if (
      !patchedShape &&
      Buffer.compare(span, Buffer.from(buildSiteBOriginal(siteBSymbols), 'latin1')) === 0
    ) {
      siteB = 'unpatched';
    } else if (
      patchedShape &&
      Buffer.compare(span, buildSiteBReplacement(span, siteBSymbols)) === 0
    ) {
      siteB = 'patched';
    } else if (
      patchedShape &&
      Buffer.compare(span, buildSiteBLegacyReplacement(span, siteBSymbols)) === 0
    ) {
      siteB = 'unpatched'; // migrate legacy
    } else {
      problems.push(
        `site B: Antigravity strategy span is ${span.length} bytes and is not a supported state`,
      );
    }
  }

  // Site C
  const text = buffer.toString('latin1');
  const cStates = [];
  for (const fn of SITE_C_VARIANTS) {
    const unpatchedStart = `  async${fn}(e, t) {\n` + SITE_C_BODY;
    for (let at = text.indexOf(unpatchedStart); at !== -1; at = text.indexOf(unpatchedStart, at + 1)) {
      const helperStart = at + unpatchedStart.length;
      const helperEnd = text.indexOf(SITE_C_END, helperStart);
      const helper = helperEnd === -1 ? '' : text.slice(helperStart, helperEnd);
      if (/^[$\w]+$/.test(helper)) {
        cStates.push({ kind: 'original', text: unpatchedStart + helper + SITE_C_END, fn, helper });
      }
    }
    const patchedStart = `  async${fn}(e,t){\n`;
    for (let at = text.indexOf(patchedStart); at !== -1; at = text.indexOf(patchedStart, at + 1)) {
      const window = text.slice(at, at + 512);
      const helper = /const o=([$\w]+)\(s,this\.#e\.thinkingLevel\(\)\); {2}/.exec(window)?.[1];
      if (!helper) continue;
      for (const kind of ['patched', 'legacy']) {
        const candidate = buildSiteCReplacement(fn, helper, kind === 'patched');
        if (text.startsWith(candidate, at)) {
          cStates.push({ kind, text: candidate, fn, helper });
        }
      }
    }
  }
  if (cStates.length > 1) {
    problems.push(
      'site C: usage-aware preflight guard occurs more than once; refusing ambiguous patch',
    );
  } else if (cStates.length === 1) {
    const [state] = cStates;
    if (state.kind === 'patched') {
      siteC = 'patched';
    } else {
      siteCOffset = buffer.indexOf(state.text);
      siteCReplacementText = buildSiteCReplacement(state.fn, state.helper);
    }
  } else {
    problems.push(
      'site C: usage-aware runtime preflight guard not found; upstream drift, needs review',
    );
  }

  // Site D
  for (const desc of SITE_D_VARIANTS) {
    const needles = [desc.start, `async${desc.fn}(e,t,s,o){\n`];
    let found = false;
    for (const needle of needles) {
      for (let at = buffer.indexOf(needle); at !== -1; at = buffer.indexOf(needle, at + 1)) {
        const end = buffer.indexOf(desc.endAnchor, at);
        if (end !== -1 && end - at < 2000) {
          siteDDesc = desc;
          siteDSpan = { start: at, end };
          found = true;
          break;
        }
      }
      if (found) break;
    }

    if (found) {
      const span = buffer.subarray(siteDSpan.start, siteDSpan.end);
      const patchedShape = span.indexOf(Buffer.from(`async${desc.fn}(e,t,s,o){\n`, 'latin1')) === 0;
      siteDSymbols = extractSiteDSymbols(span, patchedShape);

      if (!siteDSymbols) {
        problems.push(`site D: could not extract current minified selector symbols for ${desc.version}`);
      } else if (
        !patchedShape &&
        Buffer.compare(span, Buffer.from(buildSiteDOriginal(desc, siteDSymbols), 'latin1')) === 0
      ) {
        siteD = 'unpatched';
      } else if (
        patchedShape &&
        Buffer.compare(span, buildSiteDReplacement(span, desc, siteDSymbols)) === 0
      ) {
        siteD = 'patched';
      } else if (
        patchedShape &&
        Buffer.compare(span, buildSiteDLegacyReplacement(span, desc, siteDSymbols)) === 0
      ) {
        siteD = 'unpatched'; // migrate legacy sticky-without-session-boundary
      } else {
        problems.push(
          `site D: API-key selector span is ${span.length} bytes and is not a supported state for ${desc.version}`,
        );
      }
      break;
    }
  }
  if (!siteDDesc) {
    problems.push('site D: API-key selector span not found; upstream drift, needs review');
  }

  return {
    ok: problems.length === 0,
    problems,
    siteA,
    siteASpan,
    siteADesc,
    siteB,
    siteBSpan,
    siteBSymbols,
    siteC,
    siteCOffset,
    siteCReplacementText,
    siteD,
    siteDSpan,
    siteDDesc,
    siteDSymbols,
  };
}

function assertExpectedBinary(buffer, label) {
  const text = buffer.toString('latin1');
  const hasSiteA =
    text.includes('  #Vt(e, t, s) {') ||
    text.includes('  #dt(e, t, s) {') ||
    text.includes('  #qt(e, t, s) {');
  const hasSiteD =
    text.includes('  #Te(e) {') || text.includes('  #ve(e) {') || text.includes('  #Ce(e) {');
  const hasSticky = text.includes('session:sticky:');
  const hasAntigravity = text.includes('google-antigravity');

  if (!hasSiteA || !hasSiteD || !hasSticky || !hasAntigravity) {
    const missing = [];
    if (!hasSiteA) missing.push('Site A end anchor (#Vt, #dt or #qt)');
    if (!hasSiteD) missing.push('Site D end anchor (#Te, #ve or #Ce)');
    if (!hasSticky) missing.push('session:sticky:');
    if (!hasAntigravity) missing.push('google-antigravity');
    throw new Error(
      `${label} does not look like the expected omp build (missing: ${missing.join(', ')})`,
    );
  }
}

async function ensureCurrentBackup(target, backup, current) {
  try {
    const existing = await readFile(backup);
    if (Buffer.compare(existing, current) === 0) {
      return 'kept';
    }
    // A patch migration starts from an already-patched binary. Preserve the
    // pristine backup when it is the same supported upstream build instead of
    // replacing it with the previous patch generation.
    const saved = inspect(existing);
    const active = inspect(current);
    if (
      saved.ok &&
      active.ok &&
      existing.length === current.length &&
      saved.siteADesc?.version === active.siteADesc?.version &&
      saved.siteDDesc?.version === active.siteDDesc?.version
    ) {
      return 'kept-pristine';
    }
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await copyFile(target, backup);
    return 'created';
  }
  await copyFile(target, backup);
  return 'refreshed';
}

async function replaceExecutable(target, contents) {
  const temporary = `${target}.balance-patched-${process.pid}`;
  let renamed = false;
  try {
    try {
      await unlink(temporary);
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
    const handle = await open(
      temporary,
      constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL,
      0o755,
    );
    try {
      await handle.writeFile(contents);
    } finally {
      await handle.close();
    }
    await rename(temporary, target);
    renamed = true;
  } finally {
    if (!renamed) {
      await unlink(temporary).catch(() => {});
    }
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args?.help) {
    process.stdout.write(usage());
    return;
  }

  const scriptDir = path.dirname(fileURLToPath(import.meta.url));
  const fallbackTarget = path.resolve(scriptDir, '../.runtime/omp');
  let defaultTarget;
  try {
    await access(fallbackTarget, constants.R_OK);
    defaultTarget = fallbackTarget;
  } catch {
    defaultTarget = undefined;
  }

  const target = args.target ?? process.env.OMP_RUNTIME_BIN ?? defaultTarget;
  if (!target) {
    throw new Error('path-to-omp is required (or set OMP_RUNTIME_BIN)');
  }

  await access(target, constants.R_OK);
  const original = await readFile(target);
  assertExpectedBinary(original, target);

  const result = inspect(original);
  if (!result.ok) {
    throw new Error(`upstream drift:\n${result.problems.map((p) => `  ${p}`).join('\n')}`);
  }

  const pending =
    (result.siteA === 'unpatched' ? 1 : 0) +
    (result.siteB === 'unpatched' ? 1 : 0) +
    (result.siteC === 'unpatched' ? 1 : 0) +
    (result.siteD === 'unpatched' ? 1 : 0);

  if (pending === 0) {
    process.stdout.write(`already patched: all sites applied in ${target}\n`);
    return;
  }

  if (args.check) {
    const parts = [];
    if (result.siteA === 'unpatched') parts.push('site A (Antigravity-scoped headroom)');
    if (result.siteB === 'unpatched') parts.push('site B (antigravity findWindowLimits)');
    if (result.siteC === 'unpatched') parts.push('site C (empty-transcript usage fallback)');
    if (result.siteD === 'unpatched') parts.push('site D (request-scoped API-key session sticky)');
    process.stdout.write(`unpatched: ${parts.join(', ')} in ${target}\n`);
    process.exitCode = 1;
    return;
  }

  const patched = Buffer.from(original);
  const notes = [];

  if (result.siteA === 'unpatched') {
    const { start, end } = result.siteASpan;
    const replacement = buildSiteAReplacement(patched.subarray(start, end), result.siteADesc);
    if (replacement.length !== end - start) {
      throw new Error('internal error: site A replacement changed length');
    }
    replacement.copy(patched, start);
    notes.push(`site A at ${start}..${end}: pure headroom ranking for Antigravity`);
  }

  if (result.siteB === 'unpatched') {
    const { start, end } = result.siteBSpan;
    const replacement = buildSiteBReplacement(patched.subarray(start, end), result.siteBSymbols);
    if (replacement.length !== end - start) {
      throw new Error('internal error: site B replacement changed length');
    }
    replacement.copy(patched, start);
    notes.push(`site B at ${start}..${end}: primary 5h window ranking`);
  }

  if (result.siteC === 'unpatched') {
    patched.write(result.siteCReplacementText, result.siteCOffset, 'latin1');
    notes.push(`site C at ${result.siteCOffset}: usage fallback limited to empty transcripts`);
  }

  if (result.siteD === 'unpatched') {
    const { start, end } = result.siteDSpan;
    const replacement = buildSiteDReplacement(patched.subarray(start, end), result.siteDDesc, result.siteDSymbols);
    if (replacement.length !== end - start) {
      throw new Error('internal error: site D replacement changed length');
    }
    replacement.copy(patched, start);
    notes.push(`site D at ${start}..${end}: only current-session API-key sticky wins before ranking`);
  }

  if (patched.length !== original.length) {
    throw new Error('internal error: patch changed the file length');
  }

  const backup = `${target}.balance-orig`;
  const backupState = await ensureCurrentBackup(target, backup, original);
  await replaceExecutable(target, patched);

  process.stdout.write(
    `patched balance rules in ${target}\n` +
      notes.map((n) => `  ${n}\n`).join('') +
      `  backup: ${backup} (${backupState})\n` +
      'Rules active:\n' +
      '  - Site A & B: highest-5h Antigravity account picked for new sessions (rule 1).\n' +
      '  - Site C: established sessions keep sticky model, bypassing 15% reserve preflight (rule 2/3).\n' +
      '  - Site D: fresh UUID sessions rank once, then preserve account stickiness until blocked (rule 3).\n',
  );
}

main().catch((error) => {
  process.stderr.write(`patch-omp-antigravity-balance: ${error.message}\n`);
  process.exit(1);
});
