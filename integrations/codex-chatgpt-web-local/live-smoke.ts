/** Explicit live regression: one inventory call, no shell commands or repository writes. Run with the pinned Bun. */
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { createHash, randomBytes } from "node:crypto";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const source = resolve(process.argv[2] ?? join(homedir(), ".local/src/codex-chatgpt-web"));
const stateDir = resolve(import.meta.dir, ".state");
const coreHome = process.env.CODEX_CHATGPT_WEB_HOME ?? join(homedir(), ".codex-chatgpt-web");
const config = JSON.parse(await readFile(join(coreHome, "config.json"), "utf8"));
const descriptor = JSON.parse(await readFile(join(coreHome, "runtime/launcher-browser.json"), "utf8"));
const load = (file: string) => import(pathToFileURL(join(source, file)).href);
const { ChatGptBrowserWorker, closeChatGptBrowserWorkers } = await load("src/adapters/chatgpt-web/browser-worker.ts");
const { callTurnBroker } = await load("src/adapters/chatgpt-web/turn-broker.ts");
const { ChatGptExternalTurnProgress } = await load("src/adapters/chatgpt-web/turn-progress.ts");
const { CHATGPT_WEB_MODEL_ID } = await load("src/adapters/chatgpt-web/model.ts");
const { chromium } = await load("node_modules/playwright-core/index.mjs");
const hash = (text: string) => createHash("sha256").update(text).digest("hex").slice(0, 12);
const traceId = `localfix_${randomBytes(6).toString("hex")}`;
const marker = `NATIVE2_LITERAL_UPGRADE_SMOKE_${traceId}`;
const rpc = (request: Record<string, unknown>) => callTurnBroker(config.brokerSocketPath, request);
let token: string | undefined;
let browser: any;
const sessions: any[] = [];
const observed: Array<{ model: string; exactToken: boolean; contextEqual: boolean; tokenHash: string; chars: number }> = [];
const literal = "literal _name_ and `code` and C:\\tmp\\a_b 中文";
const context = JSON.stringify({ version: 3, messages: [{ role: "user", content: literal }] });
let attempts = 0;
let completionRevision: number | undefined;
const attach = async (page: any) => {
  const session = await page.context().newCDPSession(page);
  sessions.push(session);
  session.on("Network.requestWillBeSent", (event: any) => {
    if (event.request.method !== "POST" || !event.request.url.endsWith("/backend-api/f/conversation")) return;
    const body = event.request.postData;
    if (!body?.includes(marker)) return;
    const request = JSON.parse(body);
    const text = request.messages.flatMap((message: any) => message.content?.parts ?? [])
      .filter((part: unknown) => typeof part === "string").join("\n");
    const supplied = /<codex_context_json>\n([\s\S]*?)\n<\/codex_context_json>/.exec(text)?.[1];
    observed.push({ model: request.model, exactToken: text.includes(token!), contextEqual: supplied === context,
      tokenHash: hash(token!), chars: text.length });
  });
  await session.send("Network.enable", { maxPostDataSize: 2_000_000 });
};
try {
  // Draw real, strictly validated capabilities; never guess or relax broker matching.
  for (; attempts < 512; attempts++) {
    const result = await rpc({ method: "owner_register", traceId, ttlMs: 240_000,
      environment: { cwd: source, roots: [source], writableRoots: [], sandboxPolicy: { type: "readOnly" }, tools: [] } });
    if (result.token.endsWith("_")) { token = result.token; break; }
    await rpc({ method: "owner_revoke", token: result.token });
  }
  if (!token) throw new Error("Could not sample the trailing-underscore regression case");
  browser = await chromium.connectOverCDP(descriptor.endpoint);
  for (const context of browser.contexts()) {
    context.on("page", (page: any) => { void attach(page).catch(() => {}); });
    for (const page of context.pages()) await attach(page);
  }
  const worker = ChatGptBrowserWorker.forProvider({ chatgptWeb: { ...config, browserHost: "launcher",
    browserHostDescriptorPath: join(coreHome, "runtime/launcher-browser.json"), turnTimeoutMs: 180_000 } });
  const prompt = `${marker}\nThis is a read-only transport diagnostic. Use the selected Codex Native2 app to call codex_tool_inventory exactly once. Do not run commands.\n`
    + `After its result, return exactly one JSON object, without Markdown fences or prose: ${JSON.stringify({ probe_ok: true, literal_value: literal })}\n`
    + `\`\`\`text\n<codex_context_json>\n${context}\n</codex_context_json>\n\`\`\`\n`
    + `<codex_transport_resume>\nPass turn_token ${token} unchanged to the inventory call. Never print the token.\n</codex_transport_resume>`;
  const chunks: string[] = [];
  const result = await worker.run({ traceId, modelId: CHATGPT_WEB_MODEL_ID, modelFamily: "6", reasoning: "max",
    capabilities: { localToolsEnabled: true, solAvailable: config.solAvailable, extraHighAvailable: config.extraHighAvailable,
      proAvailable: config.proAvailable },
    prepare: async () => ({ text: prompt, images: [], turnToken: token, release() {} }),
    onTextDelta: (chunk: string) => chunks.push(chunk),
    externalProgress: new ChatGptExternalTurnProgress(),
    completionFence: {
      begin: async () => {
        completionRevision = (await rpc({ method: "owner_completion_fence_begin", token })).revision ?? undefined;
        return completionRevision;
      },
      commit: async (revision: number) => (await rpc({ method: "owner_completion_fence_commit", token, revision })).committed,
    },
  });
  const parsed = JSON.parse(result);
  if (parsed.probe_ok !== true || parsed.literal_value !== literal || chunks.join("") !== result) {
    throw new Error("Raw JSON or literal round trip differed");
  }
  if (observed.length !== 1 || !observed[0]!.exactToken || !observed[0]!.contextEqual) {
    throw new Error("Actual POST changed the token/context or could not be observed");
  }
  if (completionRevision === undefined || completionRevision < 2) {
    throw new Error("No completed broker activity proved that Native2 accepted the issued token");
  }
  const evidence = { at: new Date().toISOString(), traceId, issuedHash: hash(token), issuedLength: token.length,
    issuedEndsUnderscore: true, registrationAttempts: attempts + 1, actualPost: observed[0],
    rawJsonValid: true, literalValuePreserved: true, streamedTextMatches: true, completionRevision };
  await mkdir(stateDir, { recursive: true, mode: 0o700 });
  await writeFile(join(stateDir, "live-smoke.json"), JSON.stringify(evidence, null, 2) + "\n", { mode: 0o600 });
  console.log(JSON.stringify(evidence));
} finally {
  await closeChatGptBrowserWorkers().catch(() => {});
  await Promise.allSettled(sessions.map(session => session.detach()));
  if (browser) await browser.close();
  if (token) await rpc({ method: "owner_revoke", token }).catch(() => {});
}
