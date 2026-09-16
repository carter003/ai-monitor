const TRACKED_NOTIFICATIONS = new Set([
  'turn/started',
  'turn/completed',
  'thread/settings/updated',
  'model/rerouted',
  'item/agentMessage/delta',
  'item/reasoning/summaryTextDelta',
  'item/reasoning/textDelta',
]);

function parseMessage(raw) {
  try {
    return JSON.parse(typeof raw === 'string' ? raw : raw.toString('utf8'));
  } catch {
    return undefined;
  }
}

function requestKey(id) {
  return typeof id === 'string' ? id : JSON.stringify(id);
}

function effectiveModel(settings) {
  return settings?.collaborationMode?.settings?.model ?? settings?.model;
}

export class CodexTpsObserver {
  constructor({ reporter, readThread }) {
    this.readThread = readThread;
    this.resolvingThreads = new Map();
    this.reporter = reporter;
    this.pendingRequests = new Map();
    this.closed = false;
    this.threads = new Map();
    this.pendingModels = new Map();
    this.eventSequence = 0;
    this.activeSequence = 0;
    this.rootThreadId = undefined;
    this.currentTurnId = undefined;
    this.model = undefined;
  }

  observeClientMessage(raw) {
    if (this.closed) return;
    const message = parseMessage(raw);
    if (!message || message.id === undefined || typeof message.method !== 'string') {
      return;
    }
    const sequence = ++this.eventSequence;
    if (message.method === 'turn/start') {
      this.rememberModel(message.params?.threadId, effectiveModel(message.params), sequence);
      this.activateThread(message.params?.threadId, undefined, sequence);
    }
    if (['thread/start', 'thread/resume', 'thread/fork'].includes(message.method)) {
      this.pendingRequests.set(requestKey(message.id), {
        model: effectiveModel(message.params),
        sequence,
      });
    }
  }

  observeServerMessage(raw) {
    this.observeServerEvent(parseMessage(raw));
  }

  observeServerEvent(message, sequence = ++this.eventSequence) {
    if (this.closed || !message) return;
    if (message.id !== undefined) {
      this.observeResponse(message);
      return;
    }

    const params = message.params;
    if (message.method === 'thread/closed' || message.method === 'thread/deleted') {
      this.threads.delete(params?.threadId);
      this.resolvingThreads.delete(params?.threadId);
      this.pendingModels.delete(params?.threadId);
      if (params?.threadId === this.rootThreadId) {
        this.reporter.resetSession(message.emittedAtMs);
        this.activeSequence = sequence;
        this.currentTurnId = undefined;
        this.rootThreadId = undefined;
        this.model = undefined;
      }
      return;
    }
    if (message.method === 'thread/settings/updated') {
      this.rememberModel(params?.threadId, effectiveModel(params?.threadSettings), sequence);
    }
    if (
      TRACKED_NOTIFICATIONS.has(message.method) &&
      params?.threadId &&
      !this.threads.has(params.threadId) &&
      this.readThread
    ) {
      const buffered = this.resolvingThreads.get(params.threadId);
      if (buffered) {
        buffered.characters += params.delta?.length ?? 0;
        if (buffered.messages.length >= 1_024 || buffered.characters > 256_000) {
          // Fail closed under a stalled read; never retain an unbounded output/history buffer.
          this.resolvingThreads.delete(params.threadId);
        } else buffered.messages.push({ message, sequence });
        return;
      }
      if (message.method === 'turn/started') {
        const pending = { messages: [{ message, sequence }], characters: 0 };
        this.resolvingThreads.set(params.threadId, pending);
        void Promise.resolve()
          .then(() => this.readThread(params.threadId))
          .then((thread) => {
            if (this.closed || this.resolvingThreads.get(params.threadId) !== pending) return;
            this.resolvingThreads.delete(params.threadId);
            if (thread?.id !== params.threadId) return;
            this.rememberThread(thread);
            for (const queued of pending.messages) {
              this.observeServerEvent(queued.message, queued.sequence);
            }
          })
          .catch(() => {
            if (this.resolvingThreads.get(params.threadId) === pending) {
              this.resolvingThreads.delete(params.threadId);
            }
          });
        return;
      }
    }
    if (message.method === 'thread/started') {
      this.rememberThread(params?.thread);
      return;
    }
    if (message.method === 'turn/started') {
      if (!this.activateThread(params?.threadId, message.emittedAtMs, sequence)) return;
    }
    if (!params || params.threadId !== this.rootThreadId) {
      return;
    }

    switch (message.method) {
      case 'turn/started': {
        const turnId = params.turn?.id ?? params.turnId;
        if (turnId) {
          this.currentTurnId = String(turnId);
          this.reporter.start(this.currentTurnId, this.model, message.emittedAtMs);
        }
        break;
      }
      case 'turn/completed':
        if (this.currentTurnId && params.turn?.id && params.turn.id !== this.currentTurnId) break;
        this.reporter.pause(message.emittedAtMs);
        this.currentTurnId = undefined;
        break;
      case 'thread/settings/updated':
        this.setModel(this.threads.get(params.threadId)?.model);
        break;
      case 'model/rerouted':
        if (!this.currentTurnId || params.turnId !== this.currentTurnId) break;
        if (sequence < (this.threads.get(params.threadId)?.modelSequence ?? 0)) break;
        // A per-turn reroute must not replace the configured model for the next turn.
        this.setModel(params.toModel);
        break;
      case 'item/agentMessage/delta':
        this.appendDelta(params, `agent:${params.itemId}`, params.delta, message.emittedAtMs);
        break;
      case 'item/reasoning/summaryTextDelta':
        this.appendDelta(
          params,
          `reasoning-summary:${params.itemId}:${params.summaryIndex ?? 0}`,
          params.delta,
          message.emittedAtMs,
        );
        break;
      case 'item/reasoning/textDelta':
        this.appendDelta(
          params,
          `reasoning:${params.itemId}:${params.contentIndex ?? 0}`,
          params.delta,
          message.emittedAtMs,
        );
        break;
      default:
        break;
    }
  }

  appendDelta(params, streamKey, delta, now) {
    if (this.currentTurnId && params.turnId && params.turnId !== this.currentTurnId) return;
    const generationKey = this.currentTurnId ?? params.turnId ?? params.itemId;
    if (!generationKey) {
      return;
    }
    this.reporter.append(String(generationKey), streamKey, delta, this.model, now);
  }

  observeResponse(message) {
    const pendingParams = this.pendingRequests.get(requestKey(message.id));
    if (!pendingParams) {
      return;
    }
    this.pendingRequests.delete(requestKey(message.id));

    const threadId = message.result?.thread?.id;
    if (!threadId) {
      return;
    }
    // Only a successful explicit override may supersede a previously observed model.
    if (pendingParams.model) {
      this.rememberModel(
        threadId,
        effectiveModel(message.result) ?? pendingParams.model,
        pendingParams.sequence,
      );
    }
    this.rememberThread({
      ...message.result.thread,
      model:
        effectiveModel(message.result) ??
        effectiveModel(pendingParams) ??
        message.result.thread.model,
    });
    // A late placeholder response must not steal an already active turn.
    if (!this.currentTurnId) {
      this.activateThread(threadId, undefined, pendingParams.sequence);
    }
  }

  rememberModel(threadId, model, sequence) {
    if (!threadId || !model) return;
    const thread = this.threads.get(threadId);
    const current = thread ?? this.pendingModels.get(threadId);
    if (sequence < (current?.modelSequence ?? 0)) return;
    const update = { model: String(model), modelSequence: sequence };
    if (thread) {
      Object.assign(thread, update);
    } else {
      // Retain only scalar facts until protocol metadata confirms root/subagent identity.
      this.pendingModels.delete(threadId);
      this.pendingModels.set(threadId, update);
      if (this.pendingModels.size > 1_024) {
        this.pendingModels.delete(this.pendingModels.keys().next().value);
      }
    }
  }

  rememberThread(thread) {
    if (!thread?.id) {
      return;
    }
    const observed = this.pendingModels.get(thread.id) ?? this.threads.get(thread.id);
    this.pendingModels.delete(thread.id);
    // Base snapshots must not overwrite a model already supplied by settings/turn events.
    // Resume responses may contain the entire conversation. Retain only routing metadata.
    this.threads.set(thread.id, {
      id: thread.id,
      parentThreadId: thread.parentThreadId,
      threadSource: thread.threadSource,
      source:
        typeof thread.source === 'object' && thread.source?.subAgent
          ? { subAgent: true }
          : undefined,
      model: observed?.modelSequence ? observed.model : thread.model,
      modelSequence: observed?.modelSequence ?? 0,
    });
  }

  activateThread(threadId, now, sequence) {
    const thread = this.threads.get(threadId);
    if (
      !thread ||
      thread.parentThreadId != null ||
      // Internal system threads can be parentless too; they never own the TUI model.
      thread.threadSource === 'system' ||
      (typeof thread.source === 'object' && thread.source?.subAgent) ||
      sequence < this.activeSequence
    ) {
      return false;
    }
    this.activeSequence = sequence;
    if (threadId !== this.rootThreadId) {
      const switching = this.rootThreadId !== undefined;
      this.currentTurnId = undefined;
      this.rootThreadId = threadId;
      this.model = undefined;
      if (switching) {
        if (thread.model) this.reporter.start(`thread:${threadId}`, thread.model, now);
        else this.reporter.resetSession(now);
      }
    }
    this.setModel(thread.model);
    return true;
  }

  close() {
    this.closed = true;
    this.pendingRequests.clear();
    this.resolvingThreads.clear();
    this.threads.clear();
    this.pendingModels.clear();
  }

  setModel(model) {
    if (!model || model === this.model) {
      return;
    }
    this.model = String(model);
    this.reporter.setModel(this.model);
  }
}
