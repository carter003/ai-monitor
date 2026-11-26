import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const installer = fileURLToPath(new URL('../install.mjs',import.meta.url));
function setup(t) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(),'herdr-install-'));
  t.after(()=>fs.rmSync(home,{recursive:true,force:true}));
  for (const p of ['.local/bin','.codex','.config/herdr']) fs.mkdirSync(path.join(home,p),{recursive:true});
  fs.copyFileSync('/bin/true',path.join(home,'.local/bin/omp')); fs.chmodSync(path.join(home,'.local/bin/omp'),0o755);
  fs.symlinkSync('/bin/true',path.join(home,'.local/bin/codex'));
  fs.writeFileSync(path.join(home,'.codex/config.toml'),`model = "native"\n[[hooks.SessionStart]]\n[[hooks.SessionStart.hooks]]\ntype = "command"\ncommand = "official-hook"\n[hooks.state."config:session_start:0:0"]\ntrusted = true\n`);
  fs.writeFileSync(path.join(home,'.config/herdr/config.toml'),'[ui.sidebar.agents]\nrows = [["agent", "$model", "$tps"]]\n');
  fs.writeFileSync(path.join(home,'.bashrc'),'export KEEP=1\n# >>> omp profile alias: omp2 >>>\nold\n# <<< omp profile alias: omp2 <<<\n');
  const run = (...args) => spawnSync(process.execPath,[installer,...args],{env:{...process.env,HERDR_TPS_HOME:home},encoding:'utf8'});
  return {home,run};
}
test('installer dry-run is read only, appends trusted groups in order, and uninstalls idempotently',t=>{
  const {home,run} = setup(t);
  const config = path.join(home,'.codex/config.toml');
  const original = fs.readFileSync(config,'utf8');
  assert.equal(run('--dry-run').status,0);
  assert.equal(fs.readFileSync(config,'utf8'),original);
  let result=run(); assert.equal(result.status,0,result.stderr);
  const next=fs.readFileSync(config,'utf8');
  assert.ok(next.startsWith(original.trimEnd()));
  assert.equal((next.match(/\[\[hooks.SessionStart\]\]/g)||[]).length,2);
  assert.equal((next.match(/\[\[hooks.SessionEnd\]\]/g)||[]).length,1);
  assert.equal((next.match(/trusted = true/g)||[]).length,1);
  assert.equal(fs.existsSync(path.join(home,'.codex/.hook-trust')),false);
  result=run();assert.equal(result.status,0,result.stderr);assert.equal(fs.readFileSync(config,'utf8'),next);
  assert.equal(fs.readFileSync(path.join(home,'.bashrc'),'utf8'),'export KEEP=1\n');
  result=run('--uninstall');assert.equal(result.status,0,result.stderr);
  assert.equal(fs.readFileSync(config,'utf8').trimEnd(),original.trimEnd());
  assert.ok(fs.existsSync(path.join(home,'.local/bin/omp')));
  assert.ok(fs.existsSync(path.join(home,'.local/bin/codex')));
  assert.equal(run('--uninstall').status,0);
});
test('post-write shell validation failure restores all original files and links',t=>{
  const {home,run}=setup(t);
  const config=path.join(home,'.codex/config.toml');const original=fs.readFileSync(config,'utf8');
  fs.appendFileSync(path.join(home,'.bashrc'),'if then\n');
  const result=run();assert.equal(result.status,1);
  assert.equal(fs.readFileSync(config,'utf8'),original);
  assert.equal(fs.existsSync(path.join(home,'.omp/agent/extensions/herdr-omp-tps.js')),false);
});
test('malformed TOML aborts before touching shell or native binary',t=>{
  const {home,run}=setup(t);const shell=path.join(home,'.bashrc'),original=fs.readFileSync(shell,'utf8');
  fs.appendFileSync(path.join(home,'.codex/config.toml'),'broken = [\n');
  assert.equal(run().status,1);assert.equal(fs.readFileSync(shell,'utf8'),original);
});

test('archives only managed dangling wrapper backups outside PATH and removes empty shim directory',t=>{
  const {home,run}=setup(t);
  const bin=path.join(home,'.local/bin'),shim=path.join(home,'.local/share/herdr-tps/bin');
  fs.mkdirSync(shim,{recursive:true});
  const stale=path.join(bin,'omp.bak-herdr-tps-test');
  fs.symlinkSync('/retired/herdr-tps/omp-with-tps.mjs',stale);
  fs.symlinkSync('/retired/herdr-tps/codex-with-tps.mjs',path.join(shim,'codex.bak-herdr-tps-test'));
  fs.symlinkSync('/unrelated/missing',path.join(bin,'user.bak-herdr-tps-test'));
  assert.equal(run('--dry-run').status,0);assert.ok(fs.lstatSync(stale).isSymbolicLink());
  const result=run();assert.equal(result.status,0,result.stderr);
  assert.equal(fs.existsSync(shim),false);
  assert.equal(fs.readdirSync(bin).includes(path.basename(stale)),false);
  assert.ok(fs.lstatSync(path.join(bin,'user.bak-herdr-tps-test')).isSymbolicLink());
  const archive=path.join(home,'.local/state/herdr-tps/backups');
  assert.equal(fs.readdirSync(archive,{recursive:true}).filter(name=>name.endsWith('.bak-herdr-tps-test')).length,2);
});
