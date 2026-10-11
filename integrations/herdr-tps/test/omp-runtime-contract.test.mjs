import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { access, mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { constants } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const configuredRuntime = process.env.HERDR_TPS_OMP_TEST_BIN;
const runtime = configuredRuntime
  ? path.resolve(configuredRuntime)
  : fileURLToPath(new URL('../.runtime/omp', import.meta.url));
const probe = fileURLToPath(new URL('./fixtures/omp-runtime-contract.mjs', import.meta.url));

test('installed OMP preserves native auth and session contracts', { timeout: 45_000 }, async (t) => {
  try {
    await access(runtime, constants.X_OK);
  } catch (error) {
    if (configuredRuntime) throw error;
    t.skip('No local OMP runtime; set HERDR_TPS_OMP_TEST_BIN to verify an installation');
    return;
  }

  const root = await mkdtemp(path.join(tmpdir(), 'herdr-omp-contract-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(path.join(root, 'agent'));
  const resultFile = path.join(root, 'result.json');
  // Keep the host HOME intact; all OMP state and config paths belong to this fixture.
  const env = {
    PATH: process.env.PATH,
    HOME: process.env.HOME,
    USER: process.env.USER,
    TMPDIR: process.env.TMPDIR,
    LANG: process.env.LANG,
    PI_CONFIG_DIR: path.relative(homedir(), path.join(root, 'config')),
    PI_CODING_AGENT_DIR: path.join(root, 'agent'),
    XDG_DATA_HOME: path.join(root, 'data'),
    XDG_STATE_HOME: path.join(root, 'state'),
    XDG_CACHE_HOME: path.join(root, 'cache'),
    HERDR_TPS_OMP_CONTRACT_ROOT: root,
    HERDR_TPS_OMP_CONTRACT_RESULT: resultFile,
  };
  const child = spawn(runtime, [
    '--mode', 'rpc', '--no-session', '--no-ui', '--no-tools', '--no-skills', '--no-extensions',
    '--model', 'opencode-go/deepseek-v4.1-flash', '--api-key', 'herdr-contract-fake-key',
    '--extension', probe,
  ], { cwd: root, env, stdio: ['pipe', 'ignore', 'pipe'] });
  t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); });
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-8_000); });
  const timer = setTimeout(() => child.kill('SIGKILL'), 35_000);
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  child.stdin.end();
  let exit;
  try {
    exit = await exited;
  } finally {
    clearTimeout(timer);
  }
  assert.equal(exit.code, 0, `OMP exited with ${exit.code ?? exit.signal}: ${stderr}`);
  const result = JSON.parse(await readFile(resultFile, 'utf8'));
  assert.equal(result.error, undefined, result.error);
  assert.deepEqual(result.checks, [
    'restricted stored pin delegates to native auth',
    'branch writes its own pin and resumes without ranking',
    'tree seeds TPS from the active branch',
    'first UI switch retains the model and seeded TPS',
    'reset boundary excludes earlier TPS history',
    'native context builder excludes compacted assistant history',
    'native overrides, login priority and external auth writes supersede cached keys',
  ]);
});
