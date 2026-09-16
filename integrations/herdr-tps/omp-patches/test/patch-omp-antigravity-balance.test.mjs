import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { access, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const patcher = fileURLToPath(new URL('../patch-omp-antigravity-balance.mjs', import.meta.url));

function runPatcher(args, target) {
  return spawnSync(process.execPath, [patcher, ...args, target], { encoding: 'utf8' });
}

function stickyBelongsToUuidSession(sticky, sessionId) {
  const createdAt = Number.parseInt(sessionId?.replaceAll('-', '').slice(0, 12), 16);
  return !Number.isFinite(createdAt) || sticky.lastUsedAtMs >= createdAt;
}

function fake18_2Binary() {
  const siteA =
    '  #mt(e, t, s) {\n' +
    '    const o = 1 - this.#yt(e);\n' +
    '    if (o <= 0)\n' +
    '      return 0;\n' +
    '    const n = this.#lt(e?.window);\n' +
    '    const r = e?.window?.durationMs ?? s;\n' +
    '    let i = n === undefined ? r : n - t;\n' +
    '    if (Number.isFinite(r) && r > 0) {\n' +
    '      i = Math.min(i, r);\n' +
    '    }\n' +
    '    const a = Math.max(i, 60000) / (60 * 60 * 1000);\n' +
    '    return o / a;\n' +
    '  }\n' +
    '  #dt(e, t, s) {\n';

  const siteB =
    '    id: "google-antigravity",\n    fetchUsage:\n' +
    '    findWindowLimits(e, t) {\n' +
    '      return { primary: eei(e, t)[0] };\n' +
    '    },\n' +
    '    scopeLimits: MX,\n' +
    '    blockScope(e) {\n' +
    '      const t = jz(e?.modelId);\n' +
    '      return `counter:${t ?? "unknown"}`;\n' +
    '    },\n' +
    '    healableBlockScopes(e) {';

  const siteC =
    '  async#z(e, t) {\n' +
    '    if (!this.#e.settings.get("retry.usageAwareFallback"))\n' +
    '      return false;\n' +
    '    const s = this.#e.model();\n' +
    '    if (!s)\n' +
    '      return false;\n' +
    '    const o = sR(s, this.#e.thinkingLevel());';

  const siteD =
    '  async#Q(e, t, s, o) {\n' +
    '    const n = this.#X(e).map((d, g) => ({ credential: d, index: g })).filter((d) => {\n' +
    '      if (d.credential.type !== "api_key")\n' +
    '        return false;\n' +
    '      return o?.(d.credential) ?? true;\n' +
    '    });\n' +
    '    if (n.length === 0)\n' +
    '      return;\n' +
    '    if (n.length === 1)\n' +
    '      return n[0];\n' +
    '    const r = this.#U(e, "api_key");\n' +
    '    const i = this.#$(r, t, n.length);\n' +
    '    const a = n[i[0]];\n' +
    '    const l = this.#g?.(e);\n' +
    '    if (!l) {\n' +
    '      for (const d of i) {\n' +
    '        const g = n[d];\n' +
    '        if (!this.#ce(e, r, g.index)) {\n' +
    '          return g;\n' +
    '        }\n' +
    '      }\n' +
    '      return a;\n' +
    '    }\n' +
    '    const u = {\n' +
    '      modelId: s?.modelId\n' +
    '    };\n' +
    '    const p = l.blockScope?.(u);\n' +
    '    const c = LZe(e, l, u, p);\n' +
    '    const f = await this.#fe({\n' +
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
    '    return f[0]?.selection ?? a;\n' +
    '  }\n' +
    '  #ve(e) {';

  const sticky = 'session:sticky:example';
  return Buffer.from(`${siteA}\n${siteB}\n${siteC}\n${siteD}\n${sticky}\n`, 'latin1');
}
function fake18_2_1Binary() {
  const siteA =
    '  #lt(e, t, s) {\n' +
    '    const o = 1 - this.#ct(e);\n' +
    '    if (o <= 0)\n' +
    '      return 0;\n' +
    '    const n = this.#wt(e?.window);\n' +
    '    const r = e?.window?.durationMs ?? s;\n' +
    '    let i = n === undefined ? r : n - t;\n' +
    '    if (Number.isFinite(r) && r > 0) {\n' +
    '      i = Math.min(i, r);\n' +
    '    }\n' +
    '    const a = Math.max(i, 60000) / (60 * 60 * 1000);\n' +
    '    return o / a;\n' +
    '  }\n' +
    '  #Vt(e, t, s) {\n';

  const siteB =
    '    id: "google-antigravity",\n    fetchUsage:\n' +
    '    findWindowLimits(e, t) {\n' +
    '      return { primary: dri(e, t)[0] };\n' +
    '    },\n' +
    '    scopeLimits: z8,\n' +
    '    blockScope(e) {\n' +
    '      const t = z7(e?.modelId);\n' +
    '      return `counter:${t ?? "unknown"}`;\n' +
    '    },\n' +
    '    healableBlockScopes(e) {';

  const siteC =
    '  async#X(e, t) {\n' +
    '    if (!this.#e.settings.get("retry.usageAwareFallback"))\n' +
    '      return false;\n' +
    '    const s = this.#e.model();\n' +
    '    if (!s)\n' +
    '      return false;\n' +
    '    const o = uk(s, this.#e.thinkingLevel());';

  const siteD =
    '  async#he(e, t, s, o) {\n' +
    '    const n = this.#U(e).map((f, g) => ({ credential: f, index: g })).filter((f) => {\n' +
    '      if (f.credential.type !== "api_key")\n' +
    '        return false;\n' +
    '      return o?.(f.credential) ?? true;\n' +
    '    });\n' +
    '    if (n.length === 0)\n' +
    '      return;\n' +
    '    if (n.length === 1)\n' +
    '      return n[0];\n' +
    '    const r = this.#D(e, "api_key");\n' +
    '    const i = this.#V(r, t, n.length);\n' +
    '    const a = n[i[0]];\n' +
    '    const l = this.#g?.(e);\n' +
    '    if (!l) {\n' +
    '      for (const f of i) {\n' +
    '        const g = n[f];\n' +
    '        if (!this.#ue(e, r, g.index)) {\n' +
    '          return g;\n' +
    '        }\n' +
    '      }\n' +
    '      return a;\n' +
    '    }\n' +
    '    const u = {\n' +
    '      modelId: s?.modelId\n' +
    '    };\n' +
    '    const p = l.blockScope?.(u);\n' +
    '    const c = fet(e, l, u, p);\n' +
    '    const d = await this.#le({\n' +
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
    '    return d[0]?.selection ?? a;\n' +
    '  }\n' +
    '  #Te(e) {';

  const sticky = 'session:sticky:example';
  return Buffer.from(`${siteA}\n${siteB}\n${siteC}\n${siteD}\n${sticky}\n`, 'latin1');
}

function fake18_1Binary() {
  const siteA =
    '  #Ot(e, t, s) {\n' +
    '    const o = 1 - this.#it(e);\n' +
    '    if (o <= 0)\n' +
    '      return 0;\n' +
    '    const n = this.#gt(e?.window);\n' +
    '    const r = e?.window?.durationMs ?? s;\n' +
    '    let i = n === undefined ? r : n - t;\n' +
    '    if (Number.isFinite(r) && r > 0) {\n' +
    '      i = Math.min(i, r);\n' +
    '    }\n' +
    '    const a = Math.max(i, 60000) / (60 * 60 * 1000);\n' +
    '    return o / a;\n' +
    '  }\n' +
    '  #qt(e, t, s) {\n';

  const siteB =
    '    id: "google-antigravity",\n    fetchUsage:\n' +
    '    findWindowLimits(e, t) {\n' +
    '      return { primary: eti(e, t)[0] };\n' +
    '    },\n' +
    '    scopeLimits: Mne,\n' +
    '    blockScope(e) {\n' +
    '      const t = hj(e?.modelId);\n' +
    '      return `counter:${t ?? "unknown"}`;\n' +
    '    },\n' +
    '    healableBlockScopes(e) {';

  const siteC =
    '  async#B(e, t) {\n' +
    '    if (!this.#e.settings.get("retry.usageAwareFallback"))\n' +
    '      return false;\n' +
    '    const s = this.#e.model();\n' +
    '    if (!s)\n' +
    '      return false;\n' +
    '    const o = Cb(s, this.#e.thinkingLevel());';

  const siteD =
    '  async#ne(e, t, s, o) {\n' +
    '    const n = this.#Ke(e).map((d, g) => ({ credential: d, index: g })).filter((d) => {\n' +
    '      if (d.credential.type !== "api_key")\n' +
    '        return false;\n' +
    '      return o?.(d.credential) ?? true;\n' +
    '    });\n' +
    '    if (n.length === 0)\n' +
    '      return;\n' +
    '    if (n.length === 1)\n' +
    '      return n[0];\n' +
    '    const r = this.#De(e, "api_key");\n' +
    '    const i = this.#Y(r, t, n.length);\n' +
    '    const a = n[i[0]];\n' +
    '    const l = this.#g?.(e);\n' +
    '    if (!l) {\n' +
    '      for (const d of i) {\n' +
    '        const g = n[d];\n' +
    '        if (!this.#pe(e, r, g.index)) {\n' +
    '          return g;\n' +
    '        }\n' +
    '      }\n' +
    '      return a;\n' +
    '    }\n' +
    '    const u = {\n' +
    '      modelId: s?.modelId\n' +
    '    };\n' +
    '    const p = l.blockScope?.(u);\n' +
    '    const c = p_e(e, l, u, p);\n' +
    '    const f = await this.#he({\n' +
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
    '    return f[0]?.selection ?? a;\n' +
    '  }\n' +
    '  #Ce(e) {';

  const sticky = 'session:sticky:example';
  return Buffer.from(`${siteA}\n${siteB}\n${siteC}\n${siteD}\n${sticky}\n`, 'latin1');
}

test('inspects and patches all 4 sites in v18.2 binary while preserving exact byte length', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'omp-balance-test-18-2-'));
  const target = path.join(directory, 'omp');
  const original = fake18_2Binary();
  await writeFile(target, original);

  try {
    const checkBefore = runPatcher(['--check'], target);
    assert.equal(checkBefore.status, 1);
    assert.match(checkBefore.stdout, /unpatched: site A.*site B.*site C.*site D/);

    const patchRun = runPatcher([], target);
    assert.equal(patchRun.status, 0);
    assert.match(patchRun.stdout, /patched balance rules/);

    const patched = await readFile(target);
    assert.equal(patched.length, original.length);
    assert.match(patched.toString('latin1'), /h\.lastUsedAtMs>=v/);

    const backup = await readFile(`${target}.balance-orig`);
    assert.equal(Buffer.compare(backup, original), 0);

    const checkAfter = runPatcher(['--check'], target);
    assert.equal(checkAfter.status, 0);
    assert.match(checkAfter.stdout, /already patched: all sites applied/);

    const patchIdempotent = runPatcher([], target);
    assert.equal(patchIdempotent.status, 0);
    assert.match(patchIdempotent.stdout, /already patched: all sites applied/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('a fresh UUID session rejects an inherited pin and accepts its own first-request pin', () => {
  const sessionId = '01a0a7dd-7c3b-716f-8370-bba8d5487885';
  const createdAt = Number.parseInt(sessionId.replaceAll('-', '').slice(0, 12), 16);

  assert.equal(stickyBelongsToUuidSession({ lastUsedAtMs: createdAt - 1 }, sessionId), false);
  assert.equal(stickyBelongsToUuidSession({ lastUsedAtMs: createdAt }, sessionId), true);
  assert.equal(stickyBelongsToUuidSession({ lastUsedAtMs: createdAt + 1 }, sessionId), true);
});
test('inspects and patches all 4 sites in v18.2.1 binary while preserving exact byte length', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'omp-balance-test-18-2-1-'));
  const target = path.join(directory, 'omp');
  const original = fake18_2_1Binary();
  await writeFile(target, original);

  try {
    const checkBefore = runPatcher(['--check'], target);
    assert.equal(checkBefore.status, 1);
    assert.match(checkBefore.stdout, /unpatched: site A.*site B.*site C.*site D/);

    const patchRun = runPatcher([], target);
    assert.equal(patchRun.status, 0);
    assert.match(patchRun.stdout, /patched balance rules/);

    const patched = await readFile(target);
    assert.equal(patched.length, original.length);
    assert.match(patched.toString('latin1'), /"ag5h"/);
    assert.match(patched.toString('latin1'), /h\.lastUsedAtMs>=v/);

    const backup = await readFile(`${target}.balance-orig`);
    assert.equal(Buffer.compare(backup, original), 0);

    const checkAfter = runPatcher(['--check'], target);
    assert.equal(checkAfter.status, 0);
    assert.match(checkAfter.stdout, /already patched: all sites applied/);

    const patchIdempotent = runPatcher([], target);
    assert.equal(patchIdempotent.status, 0);
    assert.match(patchIdempotent.stdout, /already patched: all sites applied/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('inspects and patches all 4 sites in v18.1 binary while preserving exact byte length', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'omp-balance-test-18-1-'));
  const target = path.join(directory, 'omp');
  const original = fake18_1Binary();
  await writeFile(target, original);

  try {
    const checkBefore = runPatcher(['--check'], target);
    assert.equal(checkBefore.status, 1);
    assert.match(checkBefore.stdout, /unpatched: site A.*site B.*site C.*site D/);

    const patchRun = runPatcher([], target);
    assert.equal(patchRun.status, 0);
    assert.match(patchRun.stdout, /patched balance rules/);

    const patched = await readFile(target);
    assert.equal(patched.length, original.length);
    assert.match(patched.toString('latin1'), /h\.lastUsedAtMs>=v/);

    const backup = await readFile(`${target}.balance-orig`);
    assert.equal(Buffer.compare(backup, original), 0);

    const checkAfter = runPatcher(['--check'], target);
    assert.equal(checkAfter.status, 0);
    assert.match(checkAfter.stdout, /already patched: all sites applied/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('refuses corrupt binary missing expected anchors', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'omp-balance-corrupt-'));
  const target = path.join(directory, 'omp');
  await writeFile(target, Buffer.from('corrupt binary content'));

  try {
    const result = runPatcher(['--check'], target);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /does not look like the expected omp build/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
