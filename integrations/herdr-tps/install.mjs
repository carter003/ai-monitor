#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { constants } from 'node:fs';
import {
  access,
  chmod,
  copyFile,
  lstat,
  mkdir,
  open,
  readFile,
  readlink,
  realpath,
  symlink,
  unlink,
  writeFile,
} from 'node:fs/promises';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dryRun = process.argv.includes('--dry-run');
const home = process.env.HERDR_TPS_HOME || homedir();
const configPath =
  process.env.HERDR_TPS_CONFIG || path.join(home, '.config', 'herdr', 'config.toml');
const binDirectory = process.env.HERDR_TPS_BIN_DIR || path.join(home, '.local', 'bin');
const commandDirectory =
  process.env.HERDR_TPS_COMMAND_DIR || path.join(home, '.local', 'share', 'herdr-tps', 'bin');
const shellRcPath =
  process.env.HERDR_TPS_SHELL_RC ||
  path.join(home, path.basename(process.env.SHELL ?? '') === 'zsh' ? '.zshrc' : '.bashrc');
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const ompRuntimeDirectory =
  process.env.HERDR_TPS_RUNTIME_DIR || path.join(scriptDirectory, '.runtime');
const managedOmpTarget = path.join(ompRuntimeDirectory, 'omp');
const originalAgentRows = `rows = [
  ["state_icon", "agent", "$model", "$tps"],
  ["workspace", "tab"],
]`;
const legacyAgentRowsWithoutStateIcon = `rows = [
  ["workspace", "tab"],
  ["agent", "$tps"],
  ["$model"],
]`;
const agentRows = `rows = [
  ["workspace", "tab"],
  ["state_icon", "agent", "$tps"],
  ["$model"],
]`;
const shellBlockStart = '# >>> herdr-tps Herdr-only PATH >>>';
const shellBlockEnd = '# <<< herdr-tps Herdr-only PATH <<<';
const legacyShellBlockStart = '# >>> herdr-tps stable Codex dispatcher PATH >>>';
const legacyShellBlockEnd = '# <<< herdr-tps stable Codex dispatcher PATH <<<';

function shellSingleQuote(value) {
  return `'${String(value).replaceAll("'", `'"'"'`)}'`;
}

function managedShellBlock() {
  return `${shellBlockStart}
if [ "\${HERDR_ENV:-}" = "1" ]; then
  export PATH=${shellSingleQuote(commandDirectory)}:"\${PATH:-}"
fi
${shellBlockEnd}`;
}

async function configuredShellRc() {
  let content = '';
  let exists = true;
  try {
    content = await readFile(shellRcPath, 'utf8');
  } catch (error) {
    if (error.code !== 'ENOENT') {
      throw error;
    }
    exists = false;
  }

  const legacyStart = content.indexOf(legacyShellBlockStart);
  const legacyEnd = content.indexOf(legacyShellBlockEnd);
  if (legacyStart >= 0 !== legacyEnd >= 0 || (legacyStart >= 0 && legacyEnd < legacyStart)) {
    throw new Error(`${shellRcPath} 中的旧 herdr-tps dispatcher PATH 标记不完整，拒绝自动修改`);
  }
  let legacyRemoved = false;
  if (legacyStart >= 0) {
    content = `${content.slice(0, legacyStart)}${content.slice(legacyEnd + legacyShellBlockEnd.length)}`;
    legacyRemoved = true;
  }

  const block = managedShellBlock();
  const start = content.indexOf(shellBlockStart);
  const end = content.indexOf(shellBlockEnd);
  const hasStart = start >= 0;
  const hasEnd = end >= 0;
  if (hasStart !== hasEnd || (hasStart && end < start)) {
    throw new Error(`${shellRcPath} 中的 herdr-tps PATH 标记不完整，拒绝自动修改`);
  }
  if (start < 0) {
    return {
      content: `${content.trimEnd()}\n\n${block}\n`,
      changed: true,
      exists,
      legacyRemoved,
    };
  }

  const blockEnd = end + shellBlockEnd.length;
  const next = `${content.slice(0, start)}${block}${content.slice(blockEnd)}`;
  return {
    content: next,
    changed: next !== content || legacyRemoved,
    exists,
    legacyRemoved,
  };
}

function unmanagedHerdrPathLines(content) {
  const findings = [];
  let insideManaged = false;
  let insideLegacy = false;
  content.split('\n').forEach((line, index) => {
    if (line.includes(shellBlockStart)) insideManaged = true;
    if (line.includes(shellBlockEnd)) insideManaged = false;
    if (line.includes(legacyShellBlockStart)) insideLegacy = true;
    if (line.includes(legacyShellBlockEnd)) insideLegacy = false;
    if (insideManaged || insideLegacy) return;
    if (/^\s*export PATH=.*herdr-tps\/bin/.test(line)) {
      findings.push(index + 1);
    }
  });
  return findings;
}

async function isNodeWrapper(target) {
  const handle = await open(target, 'r');
  try {
    const buffer = Buffer.alloc(64);
    const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0);
    return buffer.toString('utf8', 0, bytesRead).startsWith('#!/usr/bin/env node\n');
  } finally {
    await handle.close();
  }
}

async function assertNodeWrapper(target) {
  if (!(await isNodeWrapper(target))) {
    throw new Error(`${target} 不是有效的 Node wrapper，拒绝安装命令链接`);
  }
}

async function configuredContent() {
  const content = await readFile(configPath, 'utf8');
  const sectionPattern = /^\[ui\.sidebar\.agents\]\s*$/m;
  const match = sectionPattern.exec(content);
  if (!match) {
    return {
      content: `${content.trimEnd()}\n\n[ui.sidebar.agents]\n${agentRows}\n`,
      changed: true,
    };
  }

  const sectionStart = match.index + match[0].length;
  const nextSection = /^\[/m.exec(content.slice(sectionStart));
  const sectionEnd = nextSection ? sectionStart + nextSection.index : content.length;
  const section = content.slice(sectionStart, sectionEnd);
  if (section.includes(agentRows)) {
    return { content, changed: false };
  }
  const managedRows = section.includes(originalAgentRows)
    ? originalAgentRows
    : section.includes(legacyAgentRowsWithoutStateIcon)
      ? legacyAgentRowsWithoutStateIcon
      : undefined;
  if (managedRows) {
    return {
      content: `${content.slice(0, sectionStart)}${section.replace(managedRows, agentRows)}${content.slice(sectionEnd)}`,
      changed: true,
    };
  }
  if (/^\s*rows\s*=/m.test(section)) {
    const requiredTokens = ['"state_icon"', '"$tps"', '"$model"'];
    if (requiredTokens.every((token) => section.includes(token))) {
      return { content, changed: false };
    }
    throw new Error(`${configPath} 已定义自定义 ui.sidebar.agents.rows，拒绝自动覆盖`);
  }

  return {
    content: `${content.slice(0, sectionStart)}\n${agentRows}${content.slice(sectionStart)}`,
    changed: true,
  };
}

async function installLink(name, target, directory = binDirectory) {
  const linkPath = path.join(directory, name);
  try {
    const stats = await lstat(linkPath);
    if (!stats.isSymbolicLink()) {
      throw new Error(`${linkPath} 已存在且不是本工具创建的链接`);
    }
    const currentTarget = await linkTarget(linkPath);
    if (currentTarget === target) return false;

    let dangling = false;
    try {
      await access(currentTarget);
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      dangling = true;
    }
    if (!dangling || path.basename(currentTarget) !== path.basename(target)) {
      throw new Error(`${linkPath} 已存在且不是本工具创建的链接`);
    }
    if (!dryRun) await unlink(linkPath);
  } catch (error) {
    if (error.code !== 'ENOENT') {
      throw error;
    }
  }

  if (!dryRun) {
    await mkdir(directory, { recursive: true });
    await chmod(target, 0o755);
    await symlink(target, linkPath);
  }
  return true;
}

async function executableOnPath(name, wrapperTarget) {
  const excludedDirectory = path.resolve(binDirectory);
  for (const directory of process.env.PATH?.split(path.delimiter) ?? []) {
    if (!directory || path.resolve(directory) === excludedDirectory) {
      continue;
    }
    const candidate = path.join(directory, name);
    try {
      await access(candidate, constants.X_OK);
      if (wrapperTarget && (await realpath(candidate)) === (await realpath(wrapperTarget)))
        continue;
      return candidate;
    } catch {
      // Continue searching PATH.
    }
  }
  return undefined;
}

async function usableOriginalOmp(candidate, rejectedTarget) {
  if (!candidate || candidate === rejectedTarget || candidate === managedOmpTarget) {
    return undefined;
  }
  try {
    await access(candidate, constants.X_OK);
  } catch {
    return undefined;
  }
  if (await isNodeWrapper(candidate)) {
    return undefined;
  }
  return candidate;
}

async function executableTarget(linkPath, rejectedTarget) {
  try {
    const stats = await lstat(linkPath);
    if (!stats.isSymbolicLink()) {
      return undefined;
    }
    return usableOriginalOmp(await linkTarget(linkPath), rejectedTarget);
  } catch {
    return undefined;
  }
}

async function ensureManagedOmpBinary(wrapperTarget) {
  try {
    await access(managedOmpTarget, constants.X_OK);
    if (!(await isNodeWrapper(managedOmpTarget))) {
      return false;
    }
    // A previous install copied a Node wrapper; re-seed from a real original.
  } catch {
    // Seed the stable runtime target below.
  }

  const commandPath = path.join(binDirectory, 'omp');
  const originalPath = path.join(binDirectory, '.herdr-omp-original');
  const source =
    (await executableTarget(commandPath, wrapperTarget)) ??
    (await executableTarget(originalPath, wrapperTarget)) ??
    (await usableOriginalOmp(await executableOnPath('omp', wrapperTarget), wrapperTarget));
  if (!source) {
    throw new Error('找不到可用于初始化稳定 runtime 的原始 omp 可执行文件');
  }

  if (!dryRun) {
    await mkdir(ompRuntimeDirectory, { recursive: true });
    try {
      await unlink(managedOmpTarget);
    } catch (error) {
      if (error.code !== 'ENOENT') {
        throw error;
      }
    }
    await copyFile(source, managedOmpTarget);
    await chmod(managedOmpTarget, 0o755);
  }
  return true;
}

async function linkTarget(linkPath) {
  const rawTarget = await readlink(linkPath);
  return path.isAbsolute(rawTarget) ? rawTarget : path.resolve(path.dirname(linkPath), rawTarget);
}

async function replaceSymlink(linkPath, target) {
  try {
    const stats = await lstat(linkPath);
    if (!stats.isSymbolicLink()) {
      throw new Error(`${linkPath} 已存在且不是符号链接，拒绝覆盖`);
    }
    if ((await linkTarget(linkPath)) === target) {
      return false;
    }
    if (!dryRun) {
      await unlink(linkPath);
    }
  } catch (error) {
    if (error.code !== 'ENOENT') {
      throw error;
    }
  }

  if (!dryRun) {
    await symlink(target, linkPath);
  }
  return true;
}

async function installManagedOmpCommand(wrapperTarget) {
  if (!dryRun) await mkdir(binDirectory, { recursive: true });
  const originalChanged = await replaceSymlink(
    path.join(binDirectory, '.herdr-omp-original'),
    managedOmpTarget,
  );
  const commandChanged = await replaceSymlink(path.join(binDirectory, 'omp'), wrapperTarget);
  return originalChanged || commandChanged;
}

async function installUpdaterSafeCodexCommand(wrapperTarget) {
  const commandPath = path.join(binDirectory, 'codex');
  const originalPath = path.join(binDirectory, '.herdr-codex-original');
  let originalTarget;

  try {
    const stats = await lstat(commandPath);
    if (!stats.isSymbolicLink()) {
      throw new Error(`${commandPath} 已存在且不是符号链接，拒绝记录 Codex 原始命令`);
    }
    const currentTarget = await linkTarget(commandPath);
    if (currentTarget !== wrapperTarget) {
      originalTarget = currentTarget;
    }
  } catch (error) {
    if (error.code !== 'ENOENT') {
      throw error;
    }
  }

  if (!originalTarget) {
    try {
      const stats = await lstat(originalPath);
      if (!stats.isSymbolicLink()) {
        throw new Error(`${originalPath} 已存在且不是符号链接，拒绝覆盖`);
      }
      originalTarget = await linkTarget(originalPath);
    } catch (error) {
      if (error.code !== 'ENOENT') {
        throw error;
      }
    }
  }

  originalTarget ??= await executableOnPath('codex', wrapperTarget);
  if (!originalTarget || (await realpath(originalTarget)) === (await realpath(wrapperTarget))) {
    throw new Error('找不到原始 codex 可执行文件');
  }

  const originalChanged = await replaceSymlink(originalPath, originalTarget);
  let releasedLegacyCommand = false;
  try {
    if ((await linkTarget(commandPath)) === wrapperTarget) {
      releasedLegacyCommand = await replaceSymlink(commandPath, originalTarget);
    }
  } catch (error) {
    if (error.code !== 'ENOENT') {
      throw error;
    }
  }
  const shimChanged = await installLink('codex', wrapperTarget, commandDirectory);
  return originalChanged || releasedLegacyCommand || shimChanged;
}

function runHerdr(...args) {
  const result = spawnSync('herdr', args, {
    encoding: 'utf8',
    env: { ...process.env, HERDR_CONFIG_PATH: configPath },
  });
  if (result.status !== 0) {
    throw new Error(
      `herdr ${args.join(' ')} 失败\n${result.stderr || result.stdout || '无错误输出'}`,
    );
  }
}

function verifyHerdrCodexShellRoute() {
  const shell = process.env.SHELL || '/bin/bash';
  const probe = spawnSync(shell, ['-ic', `. ${shellSingleQuote(shellRcPath)}; command -v codex`], {
    encoding: 'utf8',
    env: { ...process.env, HERDR_ENV: '1' },
  });
  const output = `${probe.stdout || ''}\n${probe.stderr || ''}`;
  const expected = path.join(commandDirectory, 'codex');
  if (probe.status !== 0 || probe.stdout.trim() !== expected) {
    throw new Error(
      `Herdr shell 未稳定接管 codex（期望 ${expected}）\n${output.trim() || '无探测输出'}`,
    );
  }
}

async function main() {
  const next = await configuredContent();
  const shellRc = await configuredShellRc();
  const codexTarget = path.join(scriptDirectory, 'codex-with-tps.mjs');
  const ompTarget = path.join(scriptDirectory, 'omp-with-tps.mjs');
  await Promise.all([assertNodeWrapper(codexTarget), assertNodeWrapper(ompTarget)]);
  const managedOmpChanged = await ensureManagedOmpBinary(ompTarget);
  const linksChanged = [
    managedOmpChanged,
    await installLink('codex-tps', codexTarget),
    await installLink('omp-tps', ompTarget),
    await installUpdaterSafeCodexCommand(codexTarget),
    await installManagedOmpCommand(ompTarget),
  ].some(Boolean);

  const unmanagedPathLines = unmanagedHerdrPathLines(shellRc.content);
  if (dryRun) {
    const shellState = shellRc.legacyRemoved
      ? '需迁移旧 dispatcher PATH'
      : shellRc.changed
        ? '需更新'
        : '已就绪';
    console.log(
      `[herdr-tps] dry-run: config=${next.changed ? '需更新' : '已就绪'}, shell=${shellState}, links=${linksChanged ? '需创建' : '已就绪'}`,
    );
    if (unmanagedPathLines.length > 0) {
      console.log(
        `[herdr-tps] dry-run 警告：${shellRcPath} 存在受管标记之外的 herdr-tps PATH 导出（第 ${unmanagedPathLines.join('、')} 行），请先核对用途`,
      );
    }
    return;
  }
  if (shellRc.changed) {
    await mkdir(path.dirname(shellRcPath), { recursive: true });
    let backupPath;
    if (shellRc.exists) {
      backupPath = `${shellRcPath}.bak-herdr-tps-${Date.now()}`;
      await copyFile(shellRcPath, backupPath);
    }
    await writeFile(shellRcPath, shellRc.content, 'utf8');
    const parts = [];
    if (shellRc.legacyRemoved) {
      parts.push('已移除旧全局 Codex dispatcher PATH（保留 Herdr 内条件接管与外部直通）');
    } else {
      parts.push('已更新 Herdr 专用 PATH');
    }
    console.log(
      `[herdr-tps] ${parts.join('；')}：${shellRcPath}${backupPath ? `；备份：${backupPath}` : ''}`,
    );
  }

  if (next.changed) {
    const backupPath = `${configPath}.bak-herdr-tps-${Date.now()}`;
    await copyFile(configPath, backupPath);
    await writeFile(configPath, next.content, 'utf8');
    try {
      runHerdr('config', 'check');
    } catch (error) {
      await copyFile(backupPath, configPath);
      throw error;
    }
    console.log(`[herdr-tps] 已更新 Agents 行；备份：${backupPath}`);
  } else {
    runHerdr('config', 'check');
  }

  verifyHerdrCodexShellRoute();
  runHerdr('server', 'reload-config');
  console.log(`[herdr-tps] 安装完成。重新打开 Herdr pane 后直接使用 codex 或 omp 即可。`);
  if (next.changed) {
    // 0.9.0 起 UI 渲染在客户端本地：侧栏布局属于客户端本地配置，
    // `server reload-config` 不刷新已连接客户端的侧栏。
    console.log(
      `[herdr-tps] 侧栏行已变更：在 Herdr 客户端全局菜单执行 reload config（或重启客户端）后生效。`,
    );
  }
  if (!process.env.PATH?.split(path.delimiter).includes(binDirectory)) {
    console.log(`[herdr-tps] 请把 ${binDirectory} 加入 PATH。`);
  }
}

main().catch((error) => {
  console.error(`[herdr-tps] ${error.message}`);
  process.exitCode = 1;
});
