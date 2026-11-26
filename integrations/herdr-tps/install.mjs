#!/usr/bin/env node
import fs from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { homedir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.dirname(fileURLToPath(import.meta.url));
const home = process.env.HERDR_TPS_HOME || homedir();
const bin = process.env.HERDR_TPS_BIN_DIR || path.join(home, '.local/bin');
const runtime = process.env.HERDR_TPS_RUNTIME_DIR || path.join(root, '.runtime');
const loginShell = path.basename(process.env.SHELL || 'bash');
const shell = process.env.HERDR_TPS_SHELL_RC || path.join(home, loginShell === 'zsh' ? '.zshrc' : '.bashrc');
const config = path.join(home, '.codex/config.toml');
const herdr = process.env.HERDR_TPS_CONFIG || path.join(home, '.config/herdr/config.toml');
const extensions = path.join(home, '.omp/agent/extensions');
const dry = process.argv.includes('--dry-run');
const uninstall = process.argv.includes('--uninstall');
const stamp = `${Date.now()}-${process.pid}`;
const begin = '# >>> herdr-tps native hooks >>>';
const end = '# <<< herdr-tps native hooks <<<';
const archive = path.join(home, '.local/state/herdr-tps/backups', stamp);
const changes = [];
const undo = [];
const exists = (file) => { try { fs.lstatSync(file); return true; } catch (e) { if (e.code !== 'ENOENT') throw e; return false; } };
const read = (file) => exists(file) ? fs.readFileSync(file, 'utf8') : '';
const quote = (value) => `'${value.replaceAll("'", `'"'"'`)}'`;
function strip(content, start, finish) {
  const a = content.indexOf(start), b = content.indexOf(finish);
  if ((a < 0) !== (b < 0) || b < a) throw new Error(`Incomplete managed block: ${start}`);
  if (a < 0) return content;
  if (content.indexOf(start, a + start.length) >= 0) throw new Error(`Duplicate managed block: ${start}`);
  return content.slice(0, a) + content.slice(b + finish.length).replace(/^\n/, '');
}
function plan(file, value, kind = 'file') {
  if (kind === 'file' && read(file) === value) return;
  if (kind === 'link' && exists(file) && fs.lstatSync(file).isSymbolicLink() && fs.readlinkSync(file) === value) return;
  if (kind === 'remove' && !exists(file)) return;
  changes.push({ file, value, kind });
}
function removeManagedLink(file, allowed, { keepUnmanaged = false } = {}) {
  if (!exists(file)) return;
  if (!fs.lstatSync(file).isSymbolicLink() || !allowed.some(x => fs.readlinkSync(file).includes(x))) {
    if (keepUnmanaged) return;
    throw new Error(`Refusing to remove an unmanaged entry: ${file}`);
  }
  plan(file, undefined, 'remove');
}
function check(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`${command} validation failed: ${result.stderr || result.error || result.stdout}`);
}
function toml(content, readRows = false) {
  const parse = 'import sys,tomllib,json; config = tomllib.loads(sys.stdin.read())';
  const rows = "print(json.dumps(config.get('ui', {}).get('sidebar', {}).get('agents', {}).get('rows')))";
  const result = spawnSync('python3', ['-c', readRows ? `${parse}; ${rows}` : parse], { input: content, encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`TOML validation failed: ${result.stderr}`);
  if (readRows) return JSON.parse(result.stdout);
}

const defaultRows = [['workspace', 'tab'], ['state_icon', 'agent', '$tps'], ['$model']];
const legacyRows = [
  [['state_icon', 'agent', '$model', '$tps'], ['workspace', 'tab']],
  [['workspace', 'tab'], ['agent', '$tps'], ['$model']],
];

function configureSidebar(content) {
  const rows = toml(content, true);
  const assignment = `rows = ${JSON.stringify(defaultRows)}`;
  const header = /^[ \t]*\[ui\.sidebar\.agents\][ \t]*(?:#[^\n]*)?$/m.exec(content);
  const start = header && header.index + header[0].length;
  let updated;
  if (rows === null) {
    updated = header
      ? content.slice(0, start) + '\n' + assignment + content.slice(start)
      : `${content}\n[ui.sidebar.agents]\n${assignment}\n`;
  } else if (header && legacyRows.some(layout => JSON.stringify(layout) === JSON.stringify(rows))) {
    // Only replace the simple nested string arrays emitted by earlier installers.
    const tail = content.slice(start);
    const end = tail.search(/^[ \t]*\[+[ \t]*[\w.-]+[ \t]*\]+[ \t]*(?:#.*)?$/m);
    const section = end < 0 ? tail : tail.slice(0, end);
    const migrated = section.replace(/^[ \t]*rows[ \t]*=[ \t]*\[\s*(?:\[[^\[\]#]*\]\s*,?\s*)+\]/m, () => assignment);
    if (section === migrated) return content; // A reformatted legacy layout already shows both fields.
    updated = content.slice(0, start) + migrated + tail.slice(section.length);
  } else {
    if (Array.isArray(rows) && rows.every(Array.isArray) &&
        ['$model', '$tps'].every(token => rows.flat().includes(token))) return content;
    throw new Error(`${herdr}: custom ui.sidebar.agents.rows must include $model and $tps`);
  }
  if (JSON.stringify(toml(updated, true)) !== JSON.stringify(defaultRows)) {
    throw new Error(`Cannot safely update ui.sidebar.agents.rows in ${herdr}`);
  }
  return updated;
}
function save(file) {
  if (!exists(file)) { undo.push(() => { if (exists(file)) fs.unlinkSync(file); }); return; }
  const stat = fs.lstatSync(file);
  if (stat.isSymbolicLink()) {
    const target = fs.readlinkSync(file);
    fs.mkdirSync(archive, { recursive: true, mode: 0o700 });
    const name = `${createHash('sha256').update(file).digest('hex').slice(0,12)}-${path.basename(file)}`;
    fs.symlinkSync(target, path.join(archive, name));
    undo.push(() => { if (exists(file)) fs.unlinkSync(file); fs.symlinkSync(target, file); });
  } else {
    const backup = `${file}.bak-herdr-tps-${stamp}`;
    fs.copyFileSync(file, backup); fs.chmodSync(backup, stat.mode & 0o777);
    undo.push(() => { if (exists(file)) fs.unlinkSync(file); fs.copyFileSync(backup, file); fs.chmodSync(file, stat.mode & 0o777); });
  }
}
function main() {
  let shellContent = read(shell);
  for (const name of ['omp profile alias: omp2', 'herdr-tps Herdr-only PATH', 'herdr-tps stable Codex dispatcher PATH']) {
    shellContent = strip(shellContent, `# >>> ${name} >>>`, `# <<< ${name} <<<`);
  }
  plan(shell, shellContent);
  let codexContent = strip(read(config), begin, end);
  if (!uninstall) {
    const command = `${quote(process.execPath)} ${quote(path.join(root, 'codex-herdr-hook.js'))}`;
    codexContent = `${codexContent.trimEnd()}\n\n${begin}\n` + ['SessionStart', 'SessionEnd'].map((event) =>
      `[[hooks.${event}]]\n[[hooks.${event}.hooks]]\ntype = "command"\ncommand = ${JSON.stringify(`${command} ${event === 'SessionStart' ? 'start' : 'end'}`)}\ntimeout = 3\n`).join('\n') + `${end}\n`;
  }
  toml(codexContent); plan(config, codexContent);
  if (!uninstall) {
    check('flock', ['--version']);
    plan(herdr, configureSidebar(read(herdr)));
    // Validate native binary before removing any wrapper or stable runtime link.
    const omp = path.join(bin, 'omp');
    const managed = exists(omp) && fs.lstatSync(omp).isSymbolicLink() && fs.readlinkSync(omp).includes('omp-with-tps.mjs');
    if (managed || !exists(omp)) {
      const source = path.join(runtime, 'omp');
      const fd = fs.openSync(source, 'r'); const magic = Buffer.alloc(4);
      try { fs.readSync(fd, magic, 0, 4, 0); } finally { fs.closeSync(fd); }
      if (!magic.equals(Buffer.from([127, 69, 76, 70]))) throw new Error('OMP source is not an ELF binary');
      fs.accessSync(source, fs.constants.X_OK);
      check(source, ['--version']);
      plan(omp, source, 'binary');
    } else check(omp, ['--version']);
    for (const [name, target] of [['herdr-omp-subagent-model.js', 'omp-subagent-model.js'], ['herdr-omp-tps.js', 'omp-herdr-tps.js']]) {
      const file = path.join(extensions, name);
      if (exists(file) && (!fs.lstatSync(file).isSymbolicLink() || !fs.readlinkSync(file).includes('herdr-tps/'))) throw new Error(`Unmanaged extension: ${file}`);
      plan(file, path.join(root, target), 'link');
    }
  } else {
    for (const name of ['herdr-omp-subagent-model.js', 'herdr-omp-tps.js']) removeManagedLink(path.join(extensions, name), ['herdr-tps/']);
  }
  for (const name of ['.herdr-omp-original', 'omp-tps', '.herdr-codex-original', 'codex-tps']) {
    removeManagedLink(path.join(bin, name), ['herdr-tps/', '.codex/packages/standalone/'], { keepUnmanaged: true });
  }
  removeManagedLink(path.join(home, '.local/share/herdr-tps/bin/codex'), ['codex-with-tps.mjs']);
  removeManagedLink(path.join(home, '.omp/profiles/pro2/agent/extensions/herdr-omp-agent-state.ts'), ['herdr']);
  // Retired symlink backups belong outside PATH; keep the original targets for audit.
  const commandBin = path.join(home, '.local/share/herdr-tps/bin');
  for (const directory of [bin, commandBin]) {
    if (!exists(directory)) continue;
    for (const name of fs.readdirSync(directory)) {
      if (!name.includes('.bak-herdr-tps-')) continue;
      const file = path.join(directory, name);
      if (!fs.lstatSync(file).isSymbolicLink() || fs.existsSync(file)) continue;
      const target = fs.readlinkSync(file);
      if (!/herdr-tps\/(?:[^/]*-with-tps\.mjs|\.runtime\/omp)$/.test(target)) continue;
      const key = createHash('sha256').update(file).digest('hex').slice(0,12);
      plan(file, path.join(archive, `${key}-${name}`), 'archive');
    }
  }
  for (const change of changes) console.log(`[herdr-tps] ${dry ? 'dry-run ' : ''}${change.kind}: ${change.file}`);
  if (dry) return;
  try {
    for (const { file, kind, value } of changes) {
      if (kind === 'archive') {
        fs.mkdirSync(path.dirname(value), { recursive: true, mode: 0o700 });
        fs.renameSync(file, value);
        undo.push(() => fs.renameSync(value, file));
        continue;
      }
      fs.mkdirSync(path.dirname(file), { recursive: true }); save(file);
      if (kind === 'remove') { fs.unlinkSync(file); continue; }
      const temp = `${file}.tmp-herdr-tps-${stamp}`;
      if (kind === 'link') fs.symlinkSync(value, temp);
      else if (kind === 'binary') { fs.copyFileSync(value, temp); fs.chmodSync(temp, 0o755); }
      else { fs.writeFileSync(temp, value, { mode: exists(file) ? fs.statSync(file).mode & 0o777 : 0o600 }); }
      fs.renameSync(temp, file);
      if (kind === 'binary') check(file, ['--version']);
    }
    if (exists(shell)) {
      const rcName = path.basename(shell);
      const usesZsh = rcName === '.zshrc' || (rcName !== '.bashrc' && loginShell === 'zsh');
      const zsh = loginShell === 'zsh' ? process.env.SHELL : 'zsh';
      check(usesZsh ? zsh : 'bash', ['-n', shell]);
    }
    toml(read(config)); toml(read(herdr));
    if (!uninstall) check(path.join(bin, 'omp'), ['--no-tools', '--version']);
  } catch (error) { for (const restore of undo.reverse()) restore(); throw error; }
  try { fs.rmdirSync(commandBin); } catch (error) { if (!['ENOENT', 'ENOTEMPTY'].includes(error.code)) throw error; }
  console.log(`[herdr-tps] ${uninstall ? 'Uninstalled' : 'Installed'}. Backups: files .bak-herdr-tps-${stamp}; links ${archive}`);
  if (!uninstall) console.log('[herdr-tps] Open a new Codex TUI and approve the native hook trust prompt. Existing sessions stay running.');
}
try { main(); } catch (error) { console.error(`[herdr-tps] ${error.message}`); process.exitCode = 1; }
