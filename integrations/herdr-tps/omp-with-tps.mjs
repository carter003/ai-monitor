#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { existsSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { argumentValue, profileFromSessionPath } from './lib/omp-profile.mjs';
import { shareLocalOmpDaemons } from './share-omp-daemons.mjs';

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const managedOriginal = path.join(scriptDirectory, '.runtime', 'omp');
const installedOriginal = path.join(homedir(), '.local', 'bin', '.herdr-omp-original');
const ompBinary =
  process.env.HERDR_TPS_OMP_BIN ||
  (existsSync(managedOriginal)
    ? managedOriginal
    : existsSync(installedOriginal)
      ? installedOriginal
      : installedOriginal);
const extensionPath = path.join(scriptDirectory, 'omp-extension.mjs');

function forwardedArguments(args) {
  if (argumentValue(args, '--profile')) {
    return args;
  }
  const restoredProfile = profileFromSessionPath(argumentValue(args, '--resume'));
  return restoredProfile ? [`--profile=${restoredProfile}`, ...args] : args;
}

function childPath(binary) {
  if (!binary.includes(path.sep)) {
    return process.env.PATH;
  }
  try {
    const originalDirectory = path.dirname(realpathSync(binary));
    const directories = (process.env.PATH ?? '')
      .split(path.delimiter)
      .filter((directory) => directory && path.resolve(directory) !== originalDirectory);
    return [originalDirectory, ...directories].join(path.delimiter);
  } catch {
    return process.env.PATH;
  }
}

if (
  !existsSync(ompBinary) ||
  realpathSync(ompBinary) === realpathSync(fileURLToPath(import.meta.url))
) {
  console.error('[herdr-tps] 原始 OMP 不存在或指向 wrapper；请检查安装或 HERDR_TPS_OMP_BIN');
  process.exit(1);
}

const args = forwardedArguments(process.argv.slice(2));
const profile =
  argumentValue(args, '--profile') ?? process.env.OMP_PROFILE ?? process.env.PI_PROFILE;
if (
  profile?.trim() === 'pro2' &&
  !args.some((argument) => ['--help', '-h', '--version'].includes(argument))
) {
  try {
    const sharing = await shareLocalOmpDaemons({ apply: true });
    if (sharing.status === 'blocked-live-runtime') {
      console.error(
        `[herdr-tps] omp2 共享切换待完成：请先正常退出所有旧 omp2 会话，等待后台退出后重试。活动 PID: ${sharing.pids.join(', ')}`,
      );
      process.exit(2);
    }
    if (sharing.status === 'shared') {
      console.error(
        `[herdr-tps] omp2 已共用默认 OMP 后台目录。旧目录备份: ${sharing.backup ?? '无旧目录'}`,
      );
    }
  } catch (error) {
    console.error(`[herdr-tps] 无法配置 omp2 后台共享: ${error.message}`);
    process.exit(1);
  }
}

const child = spawn(ompBinary, ['--extension', extensionPath, ...args], {
  stdio: 'inherit',
  env: {
    ...process.env,
    PATH: childPath(ompBinary),
  },
});

child.once('error', (error) => {
  console.error(`[herdr-tps] 无法启动 OMP: ${error.message}`);
  process.exitCode = 1;
});

const forwardedSignals = ['SIGINT', 'SIGTERM', 'SIGHUP'];
for (const signal of forwardedSignals) {
  process.on(signal, () => {
    if (child.exitCode === null && child.signalCode === null) child.kill(signal);
  });
}

child.once('exit', (code, signal) => {
  for (const forwarded of forwardedSignals) process.removeAllListeners(forwarded);
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exitCode = code ?? 1;
});
