import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readlink,
  rm,
  symlink,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

test('preserves compatible custom rows and migrates launcher links idempotently', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-install-'));
  const configDirectory = path.join(directory, '.config', 'herdr');
  const fakeBin = path.join(directory, 'fake-bin');
  const installedBin = path.join(directory, '.local', 'bin');
  const commandBin = path.join(directory, 'herdr-command-bin');
  const runtimeBin = path.join(directory, 'runtime-bin');
  const shellRc = path.join(directory, '.bashrc');
  const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));
  await mkdir(configDirectory, { recursive: true });
  await mkdir(fakeBin, { recursive: true });
  await writeFile(
    path.join(configDirectory, 'config.toml'),
    `onboarding = false

[ui.sidebar.agents]
rows = [
  ["state_icon", "agent", "$tps", "agent_pos"],
  ["$model"],
]
`,
    'utf8',
  );
  const fakeHerdr = path.join(fakeBin, 'herdr');
  await writeFile(fakeHerdr, '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(fakeHerdr, 0o755);
  for (const command of ['codex', 'omp']) {
    const executable = path.join(fakeBin, command);
    await writeFile(executable, '#!/bin/sh\nexit 0\n', 'utf8');
    await chmod(executable, 0o755);
  }

  const env = {
    ...process.env,
    HERDR_TPS_HOME: directory,
    HERDR_TPS_BIN_DIR: installedBin,
    HERDR_TPS_COMMAND_DIR: commandBin,
    HERDR_TPS_RUNTIME_DIR: runtimeBin,
    HERDR_TPS_SHELL_RC: shellRc,
    PATH: `${fakeBin}${path.delimiter}${process.env.PATH ?? ''}`,
  };

  try {
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const result = spawnSync(process.execPath, [installer], { encoding: 'utf8', env });
      assert.equal(result.status, 0, result.stderr);
      if (attempt === 0) {
        for (const [directory, name] of [
          [installedBin, 'codex-tps'],
          [installedBin, 'omp-tps'],
          [commandBin, 'codex'],
        ]) {
          await rm(path.join(directory, name));
          const wrapperName = name.startsWith('codex')
            ? 'codex-with-tps.mjs'
            : 'omp-with-tps.mjs';
          await symlink(
            path.join(directory, 'removed-owner', wrapperName),
            path.join(directory, name),
          );
        }
      }
    }

    const config = await readFile(path.join(configDirectory, 'config.toml'), 'utf8');
    assert.match(config, /\["state_icon", "agent", "\$tps", "agent_pos"\]/);
    assert.match(config, /\["\$model"\]/);
    assert.equal((await lstat(path.join(installedBin, 'codex-tps'))).isSymbolicLink(), true);
    assert.equal((await lstat(path.join(installedBin, 'omp-tps'))).isSymbolicLink(), true);
    assert.equal((await lstat(path.join(commandBin, 'codex'))).isSymbolicLink(), true);
    assert.equal((await lstat(path.join(installedBin, 'omp'))).isSymbolicLink(), true);
    assert.equal(
      await readlink(path.join(commandBin, 'codex')),
      installer.replace(/install\.mjs$/, 'codex-with-tps.mjs'),
    );
    const shellConfig = await readFile(shellRc, 'utf8');
    assert.match(shellConfig, /herdr-tps Herdr-only PATH/);
    assert.match(shellConfig, new RegExp(commandBin.replaceAll('/', '\\/')));
    const herdrResolution = spawnSync(
      '/bin/bash',
      ['-c', '. "$HERDR_TPS_SHELL_RC"; command -v codex'],
      {
        encoding: 'utf8',
        env: { ...env, HERDR_ENV: '1', PATH: fakeBin },
      },
    );
    assert.equal(herdrResolution.status, 0, herdrResolution.stderr);
    assert.equal(herdrResolution.stdout.trim(), path.join(commandBin, 'codex'));
    const normalResolution = spawnSync(
      '/bin/bash',
      ['-c', '. "$HERDR_TPS_SHELL_RC"; command -v codex'],
      {
        encoding: 'utf8',
        env: { ...env, HERDR_ENV: '', PATH: fakeBin },
      },
    );
    assert.equal(normalResolution.status, 0, normalResolution.stderr);
    assert.equal(normalResolution.stdout.trim(), path.join(fakeBin, 'codex'));
    assert.equal((await lstat(path.join(runtimeBin, 'omp'))).isFile(), true);
    assert.equal(
      await readlink(path.join(installedBin, '.herdr-omp-original')),
      path.join(runtimeBin, 'omp'),
    );
  } finally {
    await rm(directory, { recursive: true });
  }
});

test('migrates the legacy global dispatcher PATH to the Herdr-only block', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-legacy-'));
  const configDirectory = path.join(directory, '.config', 'herdr');
  const fakeBin = path.join(directory, 'fake-bin');
  const installedBin = path.join(directory, '.local', 'bin');
  const commandBin = path.join(directory, 'herdr-command-bin');
  const runtimeBin = path.join(directory, 'runtime-bin');
  const shellRc = path.join(directory, '.bashrc');
  const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));
  await mkdir(configDirectory, { recursive: true });
  await mkdir(fakeBin, { recursive: true });
  await writeFile(
    path.join(configDirectory, 'config.toml'),
    `onboarding = false\n\n[ui.sidebar.agents]\nrows = [\n  ["workspace", "tab"],\n  ["state_icon", "agent", "$tps"],\n  ["$model"],\n]\n`,
    'utf8',
  );
  const fakeHerdr = path.join(fakeBin, 'herdr');
  await writeFile(fakeHerdr, '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(fakeHerdr, 0o755);
  for (const command of ['codex', 'omp']) {
    const executable = path.join(fakeBin, command);
    await writeFile(executable, '#!/bin/sh\nexit 0\n', 'utf8');
    await chmod(executable, 0o755);
  }
  const legacyBlock = `# >>> herdr-tps stable Codex dispatcher PATH >>>\nexport PATH='${commandBin}':"\${PATH:-}"\n# <<< herdr-tps stable Codex dispatcher PATH <<<\n`;
  const shellSeed = `${legacyBlock}\n# unrelated user content\n`;
  await writeFile(shellRc, shellSeed, 'utf8');
  const env = {
    ...process.env,
    HERDR_TPS_HOME: directory,
    HERDR_TPS_BIN_DIR: installedBin,
    HERDR_TPS_COMMAND_DIR: commandBin,
    HERDR_TPS_RUNTIME_DIR: runtimeBin,
    HERDR_TPS_SHELL_RC: shellRc,
    PATH: `${fakeBin}${path.delimiter}${process.env.PATH ?? ''}`,
  };

  try {
    const dry = spawnSync(process.execPath, [installer, '--dry-run'], { encoding: 'utf8', env });
    assert.equal(dry.status, 0, dry.stderr);
    assert.match(dry.stdout, /需迁移旧 dispatcher PATH/);
    assert.equal(await readFile(shellRc, 'utf8'), shellSeed);

    const result = spawnSync(process.execPath, [installer], { encoding: 'utf8', env });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /已移除旧全局 Codex dispatcher PATH/);
    const shellConfig = await readFile(shellRc, 'utf8');
    assert.doesNotMatch(shellConfig, /stable Codex dispatcher PATH/);
    assert.match(shellConfig, /herdr-tps Herdr-only PATH/);
    assert.match(shellConfig, /# unrelated user content/);
  } finally {
    await rm(directory, { recursive: true });
  }
});

test('refuses to rewrite a shell rc with incomplete legacy dispatcher markers', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-legacy-markers-'));
  const configDirectory = path.join(directory, '.config', 'herdr');
  const fakeBin = path.join(directory, 'fake-bin');
  const installedBin = path.join(directory, '.local', 'bin');
  const commandBin = path.join(directory, 'herdr-command-bin');
  const runtimeBin = path.join(directory, 'runtime-bin');
  const shellRc = path.join(directory, '.bashrc');
  const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));
  await mkdir(configDirectory, { recursive: true });
  await mkdir(fakeBin, { recursive: true });
  await writeFile(
    path.join(configDirectory, 'config.toml'),
    `onboarding = false\n\n[ui.sidebar.agents]\nrows = [\n  ["workspace", "tab"],\n  ["state_icon", "agent", "$tps"],\n  ["$model"],\n]\n`,
    'utf8',
  );
  const fakeHerdr = path.join(fakeBin, 'herdr');
  await writeFile(fakeHerdr, '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(fakeHerdr, 0o755);
  for (const command of ['codex', 'omp']) {
    const executable = path.join(fakeBin, command);
    await writeFile(executable, '#!/bin/sh\nexit 0\n', 'utf8');
    await chmod(executable, 0o755);
  }
  const partial = `# >>> herdr-tps stable Codex dispatcher PATH >>>\nexport PATH='${commandBin}':"\${PATH:-}"\n`;
  await writeFile(shellRc, partial, 'utf8');
  const env = {
    ...process.env,
    HERDR_TPS_HOME: directory,
    HERDR_TPS_BIN_DIR: installedBin,
    HERDR_TPS_COMMAND_DIR: commandBin,
    HERDR_TPS_RUNTIME_DIR: runtimeBin,
    HERDR_TPS_SHELL_RC: shellRc,
    PATH: `${fakeBin}${path.delimiter}${process.env.PATH ?? ''}`,
  };

  try {
    const dry = spawnSync(process.execPath, [installer, '--dry-run'], { encoding: 'utf8', env });
    assert.notEqual(dry.status, 0);
    assert.match(dry.stderr, /旧 herdr-tps dispatcher PATH 标记不完整/);
    assert.equal(await readFile(shellRc, 'utf8'), partial);
  } finally {
    await rm(directory, { recursive: true });
  }
});

test('keeps the Herdr Codex shim stable when the standalone installer replaces its alias', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-codex-update-'));
  const configDirectory = path.join(directory, '.config', 'herdr');
  const fakeBin = path.join(directory, 'fake-bin');
  const installedBin = path.join(directory, '.local', 'bin');
  const commandBin = path.join(directory, 'herdr-command-bin');
  const runtimeBin = path.join(directory, 'runtime-bin');
  const shellRc = path.join(directory, '.bashrc');
  const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));
  const wrapper = fileURLToPath(new URL('../codex-with-tps.mjs', import.meta.url));
  await mkdir(configDirectory, { recursive: true });
  await mkdir(fakeBin, { recursive: true });
  await mkdir(installedBin, { recursive: true });
  await writeFile(path.join(configDirectory, 'config.toml'), 'onboarding = false\n', 'utf8');
  await writeFile(path.join(fakeBin, 'herdr'), '#!/bin/sh\nexit 0\n', 'utf8');
  await writeFile(path.join(fakeBin, 'omp'), '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(path.join(fakeBin, 'herdr'), 0o755);
  await chmod(path.join(fakeBin, 'omp'), 0o755);
  const codexV1 = path.join(fakeBin, 'codex-v1');
  const codexV2 = path.join(fakeBin, 'codex-v2');
  await writeFile(codexV1, '#!/bin/sh\nexit 0\n', 'utf8');
  await writeFile(codexV2, '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(codexV1, 0o755);
  await chmod(codexV2, 0o755);
  await symlink(wrapper, path.join(installedBin, 'codex'));
  await symlink(codexV1, path.join(installedBin, '.herdr-codex-original'));

  const env = {
    ...process.env,
    HERDR_TPS_HOME: directory,
    HERDR_TPS_BIN_DIR: installedBin,
    HERDR_TPS_COMMAND_DIR: commandBin,
    HERDR_TPS_RUNTIME_DIR: runtimeBin,
    HERDR_TPS_SHELL_RC: shellRc,
    PATH: `${fakeBin}${path.delimiter}${process.env.PATH ?? ''}`,
  };

  try {
    let result = spawnSync(process.execPath, [installer], { encoding: 'utf8', env });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(await readlink(path.join(installedBin, 'codex')), codexV1);
    assert.equal(await readlink(path.join(commandBin, 'codex')), wrapper);

    await rm(path.join(installedBin, 'codex'));
    await symlink(codexV2, path.join(installedBin, 'codex'));
    result = spawnSync(process.execPath, [installer], { encoding: 'utf8', env });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(await readlink(path.join(installedBin, 'codex')), codexV2);
    assert.equal(await readlink(path.join(installedBin, '.herdr-codex-original')), codexV2);
    assert.equal(await readlink(path.join(commandBin, 'codex')), wrapper);
  } finally {
    await rm(directory, { recursive: true });
  }
});

test('does not seed managed omp from a leftover Node wrapper', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-wrapper-'));
  const configDirectory = path.join(directory, '.config', 'herdr');
  const fakeBin = path.join(directory, 'fake-bin');
  const installedBin = path.join(directory, '.local', 'bin');
  const runtimeBin = path.join(directory, 'runtime-bin');
  const leftoverDir = path.join(directory, 'old-herdr-tps');
  const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));
  await mkdir(configDirectory, { recursive: true });
  await mkdir(fakeBin, { recursive: true });
  await mkdir(installedBin, { recursive: true });
  await mkdir(leftoverDir, { recursive: true });
  await writeFile(path.join(configDirectory, 'config.toml'), 'onboarding = false\n', 'utf8');
  const fakeHerdr = path.join(fakeBin, 'herdr');
  await writeFile(fakeHerdr, '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(fakeHerdr, 0o755);
  const realOmp = path.join(fakeBin, 'omp');
  await writeFile(realOmp, '#!/bin/sh\necho real-omp\n', 'utf8');
  await chmod(realOmp, 0o755);
  await writeFile(path.join(fakeBin, 'codex'), '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(path.join(fakeBin, 'codex'), 0o755);

  const leftoverWrapper = path.join(leftoverDir, 'omp-with-tps.mjs');
  await writeFile(leftoverWrapper, '#!/usr/bin/env node\nconsole.log("stale-wrapper");\n', 'utf8');
  await chmod(leftoverWrapper, 0o755);
  await mkdir(runtimeBin, { recursive: true });
  await writeFile(
    path.join(runtimeBin, 'omp'),
    '#!/usr/bin/env node\nconsole.log("stale-managed");\n',
    'utf8',
  );
  await chmod(path.join(runtimeBin, 'omp'), 0o755);
  await symlink(leftoverWrapper, path.join(installedBin, 'omp'));

  const env = {
    ...process.env,
    HERDR_TPS_HOME: directory,
    HERDR_TPS_BIN_DIR: installedBin,
    HERDR_TPS_RUNTIME_DIR: runtimeBin,
    PATH: `${fakeBin}${path.delimiter}${process.env.PATH ?? ''}`,
  };

  try {
    const result = spawnSync(process.execPath, [installer], { encoding: 'utf8', env });
    assert.equal(result.status, 0, result.stderr);
    const managed = await readFile(path.join(runtimeBin, 'omp'), 'utf8');
    assert.equal(managed.includes('stale-managed'), false);
    assert.equal(managed.includes('stale-wrapper'), false);
    assert.match(managed, /real-omp/);
  } finally {
    await rm(directory, { recursive: true });
  }
});

test('reseeds managed omp when leftover wrapper cannot be inspected', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-unreadable-managed-'));
  const configDirectory = path.join(directory, '.config', 'herdr');
  const fakeBin = path.join(directory, 'fake-bin');
  const installedBin = path.join(directory, '.local', 'bin');
  const runtimeBin = path.join(directory, 'runtime-bin');
  const leftoverDir = path.join(directory, 'old-herdr-tps');
  const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));
  await mkdir(configDirectory, { recursive: true });
  await mkdir(fakeBin, { recursive: true });
  await mkdir(installedBin, { recursive: true });
  await mkdir(leftoverDir, { recursive: true });
  await mkdir(runtimeBin, { recursive: true });
  await writeFile(path.join(configDirectory, 'config.toml'), 'onboarding = false\n', 'utf8');
  const fakeHerdr = path.join(fakeBin, 'herdr');
  await writeFile(fakeHerdr, '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(fakeHerdr, 0o755);
  const realOmp = path.join(fakeBin, 'omp');
  await writeFile(realOmp, '#!/bin/sh\necho real-omp\n', 'utf8');
  await chmod(realOmp, 0o755);
  await writeFile(path.join(fakeBin, 'codex'), '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(path.join(fakeBin, 'codex'), 0o755);

  const leftoverWrapper = path.join(leftoverDir, 'omp-with-tps.mjs');
  await writeFile(leftoverWrapper, '#!/usr/bin/env node\nconsole.log("stale-wrapper");\n', 'utf8');
  await chmod(leftoverWrapper, 0o755);
  const managedOmp = path.join(runtimeBin, 'omp');
  await writeFile(managedOmp, '#!/usr/bin/env node\nconsole.log("stale-managed");\n', 'utf8');
  await chmod(managedOmp, 0o111);
  await symlink(leftoverWrapper, path.join(installedBin, 'omp'));

  const env = {
    ...process.env,
    HERDR_TPS_HOME: directory,
    HERDR_TPS_BIN_DIR: installedBin,
    HERDR_TPS_RUNTIME_DIR: runtimeBin,
    PATH: `${fakeBin}${path.delimiter}${process.env.PATH ?? ''}`,
  };

  try {
    const result = spawnSync(process.execPath, [installer], { encoding: 'utf8', env });
    assert.equal(result.status, 0, result.stderr);
    const managed = await readFile(managedOmp, 'utf8');
    assert.equal(managed.includes('stale-managed'), false);
    assert.equal(managed.includes('stale-wrapper'), false);
    assert.match(managed, /real-omp/);
  } finally {
    await rm(directory, { recursive: true });
  }
});

test('does not seed managed omp from an unreadable PATH wrapper', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-unreadable-path-'));
  const configDirectory = path.join(directory, '.config', 'herdr');
  const fakeBin = path.join(directory, 'fake-bin');
  const installedBin = path.join(directory, '.local', 'bin');
  const runtimeBin = path.join(directory, 'runtime-bin');
  const installer = fileURLToPath(new URL('../install.mjs', import.meta.url));
  await mkdir(configDirectory, { recursive: true });
  await mkdir(fakeBin, { recursive: true });
  await writeFile(path.join(configDirectory, 'config.toml'), 'onboarding = false\n', 'utf8');
  const fakeHerdr = path.join(fakeBin, 'herdr');
  await writeFile(fakeHerdr, '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(fakeHerdr, 0o755);
  await writeFile(path.join(fakeBin, 'codex'), '#!/bin/sh\nexit 0\n', 'utf8');
  await chmod(path.join(fakeBin, 'codex'), 0o755);
  const pathWrapper = path.join(fakeBin, 'omp');
  await writeFile(
    pathWrapper,
    '#!/usr/bin/env node\nconsole.log("unreadable-path-wrapper");\n',
    'utf8',
  );
  await chmod(pathWrapper, 0o111);

  const env = {
    ...process.env,
    HERDR_TPS_HOME: directory,
    HERDR_TPS_BIN_DIR: installedBin,
    HERDR_TPS_RUNTIME_DIR: runtimeBin,
    PATH: `${fakeBin}${path.delimiter}${process.env.PATH ?? ''}`,
  };

  try {
    const result = spawnSync(process.execPath, [installer], { encoding: 'utf8', env });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /EACCES|permission denied|找不到可用于初始化稳定 runtime/i);
    try {
      const managed = await readFile(path.join(runtimeBin, 'omp'), 'utf8');
      assert.equal(managed.includes('unreadable-path-wrapper'), false);
    } catch (error) {
      assert.equal(error.code, 'ENOENT');
    }
  } finally {
    await rm(directory, { recursive: true });
  }
});
