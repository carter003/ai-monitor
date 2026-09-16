import net from 'node:net';

function socketEndpoint(socketPath) {
  if (process.platform === 'win32') {
    return `\\\\.\\pipe\\${socketPath}`;
  }
  return socketPath;
}

function requestId(source) {
  return `${source}:${Date.now()}:${Math.random().toString(36).slice(2)}`;
}

export const DEFAULT_METADATA_TIMEOUT_MS = 2_000;
const HERDR_TPS_METADATA_SOURCE = 'herdr:tps';
const HERDR_TPS_LEGACY_METADATA_SOURCES = ['herdr:tps:codex', 'herdr:tps:omp'];

function responseError(response) {
  const code = response?.error?.code;
  const message = response?.error?.message;
  if (!code && !message) {
    return undefined;
  }
  return new Error(`${code ? `${code}: ` : ''}${message || 'Herdr metadata request failed'}`);
}

export class HerdrMetadataPublisher {
  constructor({
    paneId = process.env.HERDR_PANE_ID,
    socketPath = process.env.HERDR_SOCKET_PATH,
    agent,
    appliesToSource,
    timeoutMs,
    onError,
  } = {}) {
    this.paneId = paneId;
    this.socketPath = socketPath;
    this.agent = agent;
    this.appliesToSource = appliesToSource;
    const configuredTimeout = Number(timeoutMs ?? process.env.HERDR_TPS_TIMEOUT_MS);
    this.timeoutMs =
      Number.isFinite(configuredTimeout) && configuredTimeout > 0
        ? configuredTimeout
        : DEFAULT_METADATA_TIMEOUT_MS;
    this.sequences = new Map();
    this.pending = new Map();
    this.draining = false;
    this.onError = onError;
    this.lastReportedError = undefined;
    this.lastReportedErrorAt = 0;
  }

  get enabled() {
    return process.env.HERDR_ENV === '1' && Boolean(this.paneId && this.socketPath);
  }

  publishModel(model, ttlMs) {
    if (!model) {
      return Promise.resolve();
    }
    return this.#publishTokens({ model: String(model) }, ttlMs);
  }

  clearModel() {
    return this.#publishTokens({ model: null });
  }

  publishRate(rate, ttlMs) {
    return this.#publishTokens({ tps: String(rate) }, ttlMs);
  }

  publishDisplayAgent(displayAgent, ttlMs) {
    if (!displayAgent) {
      return Promise.resolve();
    }
    return this.#publishFields({ display_agent: String(displayAgent) }, ttlMs);
  }

  publishSnapshot({ model, rate, displayAgent }, ttlMs) {
    const hasTokens = model !== undefined || rate !== undefined;
    const hasDisplayAgent = Boolean(displayAgent);
    if (!hasTokens && !hasDisplayAgent) {
      return Promise.resolve();
    }
    const tokens = {
      ...(model === undefined ? {} : { model: String(model) }),
      ...(rate === undefined ? {} : { tps: String(rate) }),
    };
    const fields = {
      ...(hasDisplayAgent ? { display_agent: String(displayAgent) } : {}),
      ...(hasTokens ? { tokens } : {}),
    };
    return this.#publishFields(fields, ttlMs, { includeGuards: hasDisplayAgent });
  }

  #publishTokens(tokens, ttlMs) {
    return this.#publishFields({ tokens }, ttlMs, { includeGuards: false });
  }

  resetForAgent(ttlMs) {
    const clearedFields = {
      display_agent: null,
      tokens: { model: null, tps: null },
    };
    for (const source of HERDR_TPS_LEGACY_METADATA_SOURCES) {
      this.#publishFields(clearedFields, undefined, { source, includeGuards: false });
    }
    return this.#publishFields(
      {
        display_agent: null,
        tokens: { model: null, tps: '0' },
      },
      ttlMs,
      { includeGuards: false },
    );
  }

  clearAll() {
    for (const [key, entry] of this.pending) {
      if (entry.request.params.source === HERDR_TPS_METADATA_SOURCE) {
        entry.resolve(false);
        this.pending.delete(key);
      }
    }
    return this.#publishFields(
      {
        display_agent: null,
        tokens: { model: null, tps: null },
      },
      undefined,
      { includeGuards: false },
    );
  }

  #publishFields(fields, ttlMs, { source = HERDR_TPS_METADATA_SOURCE, includeGuards = true } = {}) {
    if (!this.enabled) {
      return Promise.resolve();
    }

    const sequence = (this.sequences.get(source) ?? Date.now() * 1_000) + 1;
    this.sequences.set(source, sequence);
    const request = {
      id: requestId(source),
      method: 'pane.report_metadata',
      params: {
        pane_id: this.paneId,
        source,
        ...(includeGuards && this.agent ? { agent: this.agent } : {}),
        ...(includeGuards && this.appliesToSource
          ? { applies_to_source: this.appliesToSource }
          : {}),
        ...fields,
        seq: sequence,
        ...(ttlMs === undefined ? {} : { ttl_ms: ttlMs }),
      },
    };

    // Keep only the latest unsent snapshot of each field/TTL/guard shape.
    const key = JSON.stringify([
      source,
      request.params.agent,
      request.params.applies_to_source,
      ttlMs,
      Object.keys(fields).sort(),
      Object.keys(fields.tokens ?? {}).sort(),
    ]);
    this.pending.get(key)?.resolve(false);
    this.pending.delete(key);
    const result = new Promise((resolve) => {
      this.pending.set(key, { request, resolve, expiresAt: ttlMs ? Date.now() + ttlMs : Infinity });
    });
    if (!this.draining) {
      this.draining = true;
      void this.drain();
    }
    return result;
  }

  async drain() {
    while (this.pending.size > 0) {
      const [key, entry] = this.pending.entries().next().value;
      this.pending.delete(key);
      if (Date.now() >= entry.expiresAt) {
        entry.resolve(false);
        continue;
      }
      if (entry.expiresAt !== Infinity) {
        entry.request.params.ttl_ms = Math.max(1, entry.expiresAt - Date.now());
      }
      try {
        entry.resolve(await this.send(entry.request));
      } catch (error) {
        // Even a diagnostic callback must not break best-effort delivery/cleanup.
        try {
          this.reportError(error, entry.request);
        } catch {
          /* Ignore diagnostic failures. */
        }
        entry.resolve(false);
      }
    }
    this.draining = false;
  }

  reportError(error, request) {
    const detail = error instanceof Error ? error.message : String(error);
    const key = `${request.params.source}:${detail}`;
    const now = Date.now();
    if (key === this.lastReportedError && now - this.lastReportedErrorAt < 30_000) {
      return;
    }
    this.lastReportedError = key;
    this.lastReportedErrorAt = now;
    if (this.onError) {
      this.onError(error, request);
      return;
    }
    console.error(`[herdr-tps] metadata publish failed (${request.params.source}): ${detail}`);
  }

  send(request) {
    return new Promise((resolve, reject) => {
      let settled = false;
      let input = '';
      const socket = net.createConnection(socketEndpoint(this.socketPath));
      const finish = (error, result) => {
        if (settled) {
          return;
        }
        settled = true;
        clearTimeout(timeout);
        socket.destroy();
        if (error) {
          reject(error);
          return;
        }
        resolve(result);
      };
      const timeout = setTimeout(
        () => finish(new Error(`Herdr metadata request timed out after ${this.timeoutMs}ms`)),
        this.timeoutMs,
      );

      socket.once('error', (error) => finish(error));
      socket.once('connect', () => socket.write(`${JSON.stringify(request)}\n`));
      socket.on('data', (chunk) => {
        input += chunk.toString('utf8');
        const lineEnd = input.indexOf('\n');
        if (lineEnd < 0) {
          return;
        }
        let response;
        try {
          response = JSON.parse(input.slice(0, lineEnd));
        } catch (error) {
          finish(new Error(`Invalid Herdr metadata response: ${error.message}`));
          return;
        }
        if (response?.id !== request.id) {
          finish(new Error('Herdr metadata response id mismatch'));
          return;
        }
        const error = responseError(response);
        if (error) {
          finish(error);
          return;
        }
        if (!response?.result) {
          finish(new Error('Herdr metadata response is missing result'));
          return;
        }
        finish(undefined, true);
      });
      socket.once('end', () => finish(new Error('Herdr metadata socket ended before response')));
    });
  }
}
