#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync, realpathSync } from 'node:fs';
import net from 'node:net';
import { homedir } from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import WebSocket, { WebSocketServer } from 'ws';
import { CodexTpsObserver } from './lib/codex-tps-observer.mjs';
import { HerdrMetadataPublisher } from './lib/herdr-metadata-publisher.mjs';

const host = '127.0.0.1';
const debug = process.env.HERDR_TPS_DEBUG === '1';
const installedOriginal = path.join(homedir(), '.local', 'bin', '.herdr-codex-original');
const codexBinary = process.env.HERDR_TPS_CODEX_BIN || installedOriginal;
const children = new Set();
let proxy;
let reporter;
let shutdownPromise;
const proxyConnections = new Set();

function logDebug(message) {
  if (debug) {
    console.error(`[herdr-tps] ${message}`);
  }
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, host, () => {
      const address = server.address();
      const port = typeof address === 'object' && address ? address.port : undefined;
      server.close((error) => {
        if (error || !port) {
          reject(error ?? new Error('无法分配本地端口'));
          return;
        }
        resolve(port);
      });
    });
  });
}

async function waitUntilReady(port, child, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  const url = `http://${host}:${port}/readyz`;
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null || !child.pid) {
      throw new Error(`Codex App Server 提前退出（code ${child.exitCode}）`);
    }
    try {
      const response = await fetch(url, {
        signal: AbortSignal.timeout(Math.max(1, Math.min(500, deadline - Date.now()))),
      });
      if (response.ok) {
        return;
      }
    } catch {
      // App Server may still be binding the socket.
    }
    await new Promise((resolve) => setTimeout(resolve, 75));
  }
  throw new Error('等待 Codex App Server 就绪超时');
}

function offeredProtocols(request) {
  const header = request.headers['sec-websocket-protocol'];
  if (typeof header !== 'string') {
    return undefined;
  }
  const protocols = header
    .split(',')
    .map((protocol) => protocol.trim())
    .filter(Boolean);
  return protocols.length > 0 ? protocols : undefined;
}

function closeCode(code) {
  return code === 1005 || code === 1006 ? undefined : code;
}

function startProxy(port, upstreamUrl, reporter) {
  return new Promise((resolve, reject) => {
    const server = new WebSocketServer({ host, port });
    server.once('listening', () => resolve(server));
    server.once('error', reject);

    server.on('connection', (client, request) => {
      const upstream = new WebSocket(upstreamUrl, offeredProtocols(request));
      proxyConnections.add(client);
      proxyConnections.add(upstream);
      const pending = [];
      const reads = new Map();
      const readPrefix = `herdr-tps:thread-read:${randomUUID()}:`;
      let readSequence = 0;
      const observer = new CodexTpsObserver({
        reporter,
        readThread: (threadId) =>
          new Promise((resolve, reject) => {
            const id = `${readPrefix}${++readSequence}`;
            const timer = setTimeout(() => {
              reads.delete(id);
              reject(new Error('Codex thread/read timed out'));
            }, 2_000);
            reads.set(id, { resolve, reject, timer });
            upstream.send(
              JSON.stringify({
                id,
                method: 'thread/read',
                params: { threadId, includeTurns: false },
              }),
            );
          }),
      });

      client.on('message', (data, isBinary) => {
        if (!isBinary) {
          observer.observeClientMessage(data);
        }
        if (upstream.readyState === WebSocket.OPEN) {
          upstream.send(data, { binary: isBinary });
        } else if (upstream.readyState === WebSocket.CONNECTING) {
          pending.push([data, isBinary]);
        }
      });

      upstream.on('open', () => {
        for (const [data, isBinary] of pending.splice(0)) {
          upstream.send(data, { binary: isBinary });
        }
      });

      upstream.on('message', (data, isBinary) => {
        if (!isBinary) {
          let message;
          try {
            message = JSON.parse(data.toString('utf8'));
          } catch {
            /* Forward non-JSON frames unchanged. */
          }
          const read = reads.get(message?.id);
          if (read) {
            clearTimeout(read.timer);
            reads.delete(message.id);
            if (message.result?.thread) read.resolve(message.result.thread);
            else read.reject(new Error('Codex thread/read failed'));
            return;
          }
          if (typeof message?.id === 'string' && message.id.startsWith(readPrefix)) return;
          observer.observeServerEvent(message);
        }
        if (client.readyState === WebSocket.OPEN) {
          client.send(data, { binary: isBinary });
        }
      });

      client.on('close', (code, reason) => {
        observer.close();
        proxyConnections.delete(client);
        if (upstream.readyState === WebSocket.OPEN) {
          upstream.close(closeCode(code), reason);
        } else {
          upstream.terminate();
        }
      });
      upstream.on('close', (code, reason) => {
        observer.close();
        proxyConnections.delete(upstream);
        for (const read of reads.values()) {
          clearTimeout(read.timer);
          read.reject(new Error('Codex connection closed'));
        }
        reads.clear();
        if (client.readyState === WebSocket.OPEN) {
          client.close(closeCode(code), reason);
        } else {
          client.terminate();
        }
      });

      client.on('error', (error) => logDebug(`TUI WebSocket: ${error.message}`));
      upstream.on('error', (error) => {
        logDebug(`App Server WebSocket: ${error.message}`);
        if (client.readyState === WebSocket.OPEN) {
          client.close(1011, 'Codex App Server connection failed');
        }
      });
    });
  });
}

function terminateChild(child, signal = 'SIGTERM') {
  if (child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
  }
}

function trackChild(child) {
  children.add(child);
  child.once('exit', () => children.delete(child));
  return child;
}

function runDirect(args) {
  return new Promise((resolve, reject) => {
    const child = trackChild(spawn(codexBinary, args, { stdio: 'inherit' }));
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
}

function cleanup(signal) {
  for (const connection of proxyConnections) connection.terminate();
  proxyConnections.clear();
  proxy?.close();
  for (const child of children) {
    terminateChild(child, signal);
  }
}

function shutdown(signal) {
  if (shutdownPromise) {
    return shutdownPromise;
  }
  cleanup(signal);
  shutdownPromise = Promise.resolve(reporter?.close());
  return shutdownPromise;
}

async function main() {
  if (
    !existsSync(codexBinary) ||
    realpathSync(codexBinary) === realpathSync(fileURLToPath(import.meta.url))
  ) {
    throw new Error('原始 Codex 不存在或指向 wrapper；请检查安装或 HERDR_TPS_CODEX_BIN');
  }
  const publisher = new HerdrMetadataPublisher({
    onError: (error, request) => {
      const detail = error instanceof Error ? error.message : String(error);
      logDebug(`metadata publish failed (${request?.params?.source}): ${detail}`);
    },
  });
  if (!publisher.enabled) {
    const result = await runDirect(process.argv.slice(2));
    if (result.signal) {
      process.kill(process.pid, result.signal);
      return;
    }
    process.exitCode = result.code ?? 1;
    return;
  }

  const { LiveTpsReporter } = await import('./lib/live-tps-reporter.mjs');
  reporter = new LiveTpsReporter({ publisher, rateEngine: 'token-rate-meter' });
  const [upstreamPort, proxyPort] = await Promise.all([freePort(), freePort()]);
  const upstreamUrl = `ws://${host}:${upstreamPort}`;
  const proxyUrl = `ws://${host}:${proxyPort}`;
  const appServer = trackChild(
    spawn(codexBinary, ['app-server', '--listen', upstreamUrl], {
      stdio: ['ignore', 'ignore', 'pipe'],
    }),
  );

  let appServerErrors = '';
  appServer.stderr.on('data', (chunk) => {
    const text = chunk.toString('utf8');
    appServerErrors = `${appServerErrors}${text}`.slice(-4_000);
    logDebug(text.trimEnd());
  });
  appServer.once('error', (error) => {
    appServerErrors = error.message;
  });

  try {
    await waitUntilReady(upstreamPort, appServer);
    proxy = await startProxy(proxyPort, upstreamUrl, reporter);
    logDebug(`Codex proxy ${proxyUrl} -> ${upstreamUrl}`);

    const tui = trackChild(
      spawn(codexBinary, ['--remote', proxyUrl, ...process.argv.slice(2)], {
        stdio: 'inherit',
      }),
    );
    const result = await new Promise((resolve, reject) => {
      tui.once('error', reject);
      tui.once('exit', (code, signal) => resolve({ code, signal }));
    });
    await shutdown();

    if (result.signal) {
      process.kill(process.pid, result.signal);
      return;
    }
    process.exitCode = result.code ?? 1;
  } catch (error) {
    await shutdown();
    const detail = appServerErrors.trim();
    throw new Error(detail ? `${error.message}\n${detail}` : error.message, { cause: error });
  }
}

for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.once(signal, () => {
    void shutdown(signal).finally(() => {
      process.removeAllListeners(signal);
      process.kill(process.pid, signal);
    });
  });
}

main().catch((error) => {
  console.error(`[herdr-tps] ${error.message}`);
  process.exitCode = 1;
});
