import { HerdrMetadataPublisher } from './lib/herdr-metadata-publisher.mjs';
import { LiveTpsReporter } from './lib/live-tps-reporter.mjs';
import { argumentValue, profileFromSessionPath } from './lib/omp-profile.mjs';

function messageKey(message) {
  return String(message?.id ?? message?.timestamp ?? 'assistant');
}

function messageModel(message, context) {
  return message?.model ?? context?.model?.id ?? context?.model?.name;
}

function sessionFile(context) {
  try {
    return context?.sessionManager?.getSessionFile?.();
  } catch {
    return undefined;
  }
}

export function ompDisplayAgent(context, argv = process.argv) {
  const configured = process.env.HERDR_TPS_OMP_DISPLAY_AGENT;
  if (configured) {
    return configured;
  }
  const profile = argumentValue(argv, '--profile') ?? profileFromSessionPath(sessionFile(context));
  return profile === 'pro2' ? 'omp2' : 'omp';
}

function trackedBlock(block) {
  if (block?.type === 'text' && typeof block.text === 'string') {
    return { type: 'text', text: block.text };
  }
  if (block?.type === 'thinking' && typeof block.thinking === 'string') {
    return { type: 'thinking', text: block.thinking };
  }
  return undefined;
}

function trackedEventType(event) {
  if (event?.type === 'text_delta') {
    return 'text';
  }
  if (event?.type === 'thinking_delta') {
    return 'thinking';
  }
  return undefined;
}

function streamKey(message, type, contentIndex) {
  return `${messageKey(message)}:${type}:${contentIndex}`;
}

function messageEndAt(message) {
  const startedAt = Number(message?.timestamp);
  const duration = Number(message?.duration);
  if (Number.isFinite(startedAt) && Number.isFinite(duration) && duration >= 0) {
    return startedAt + duration;
  }
  return Date.now();
}

class OmpTpsObserver {
  constructor(reporter) {
    this.reporter = reporter;
    this.seen = new Map();
    this.activeMessageKey = undefined;
    this.ended = false;
  }

  start(message, context) {
    if (message?.role !== 'assistant') {
      return;
    }
    this.activeMessageKey = messageKey(message);
    this.seen.clear();
    this.ended = false;
    this.reporter.start(this.activeMessageKey, messageModel(message, context), message.timestamp);
  }

  ensureStarted(message, context) {
    const key = messageKey(message);
    if (this.activeMessageKey !== key) {
      this.start(message, context);
    }
  }

  appendSnapshot(message, type, contentIndex, text, model) {
    const key = streamKey(message, type, contentIndex);
    const previous = this.seen.get(key) ?? '';
    if (!text.startsWith(previous)) {
      return false;
    }
    this.seen.set(key, text);
    const delta = text.slice(previous.length);
    if (!delta) {
      return true;
    }
    this.reporter.append(messageKey(message), key, delta, model);
    return true;
  }

  update(event, context) {
    const assistantEvent = event?.assistantMessageEvent;
    const message = event?.message;
    if (message?.role !== 'assistant' || !assistantEvent) {
      return;
    }

    this.ensureStarted(message, context);
    if (this.ended) {
      return;
    }
    const contentIndex = assistantEvent.contentIndex;
    const model = messageModel(message, context);
    const type = trackedEventType(assistantEvent);
    if (type && typeof assistantEvent.delta === 'string' && assistantEvent.delta) {
      const key = streamKey(message, type, contentIndex);
      this.seen.set(key, `${this.seen.get(key) ?? ''}${assistantEvent.delta}`);
      this.reporter.append(messageKey(message), key, assistantEvent.delta, model);
      return;
    }

    const endedType =
      assistantEvent.type === 'text_end'
        ? 'text'
        : assistantEvent.type === 'thinking_end'
          ? 'thinking'
          : undefined;
    if (!endedType) {
      return;
    }
    const partialContent = assistantEvent.partial?.content;
    const snapshot = Array.isArray(partialContent) ? partialContent[contentIndex] : undefined;
    const block = trackedBlock(snapshot);
    const text = typeof assistantEvent.content === 'string' ? assistantEvent.content : block?.text;
    if (typeof text === 'string') {
      this.appendSnapshot(message, endedType, contentIndex, text, model);
    }
  }

  end(event, context) {
    const message = event?.message;
    if (message?.role !== 'assistant') {
      return;
    }

    this.ensureStarted(message, context);
    const model = messageModel(message, context);
    if (Array.isArray(message.content)) {
      message.content.forEach((content, contentIndex) => {
        const block = trackedBlock(content);
        if (block) {
          this.appendSnapshot(message, block.type, contentIndex, block.text, model);
        }
      });
    }
    this.ended = true;
    this.reporter.pause(messageEndAt(message), message.duration);
  }
}

export function registerOmpTpsHandlers(pi, reporterOrFactory, { requireUi = false } = {}) {
  const reporterFactory =
    typeof reporterOrFactory === 'function' ? reporterOrFactory : () => reporterOrFactory;
  let reporter = requireUi ? undefined : reporterFactory();
  let observer = reporter ? new OmpTpsObserver(reporter) : undefined;
  let rootSession = !requireUi;
  let displayRefreshTimer;

  const activateRootSession = (context) => {
    if (rootSession) {
      return true;
    }
    if (context?.hasUI !== true) {
      return false;
    }
    rootSession = true;
    reporter = reporterFactory();
    observer = new OmpTpsObserver(reporter);
    return true;
  };

  const updateDisplayAgent = (context) => {
    reporter.setDisplayAgent?.(ompDisplayAgent(context));
    if (displayRefreshTimer) {
      clearTimeout(displayRefreshTimer);
    }
    displayRefreshTimer = setTimeout(() => {
      displayRefreshTimer = undefined;
      reporter.refreshDisplayAgent?.();
    }, 500);
    displayRefreshTimer.unref?.();
  };

  pi.on('session_start', (_event, context) => {
    if (!activateRootSession(context)) {
      return;
    }
    reporter.setModel(messageModel(undefined, context));
    updateDisplayAgent(context);
  });

  pi.on('session_switch', (_event, context) => {
    if (!activateRootSession(context)) {
      return;
    }
    reporter.setModel(messageModel(undefined, context));
    updateDisplayAgent(context);
  });

  pi.on('message_start', (event, context) => {
    if (!rootSession) {
      return;
    }
    observer.start(event?.message, context);
  });

  pi.on('message_update', (event, context) => {
    if (!rootSession) {
      return;
    }
    observer.update(event, context);
  });

  pi.on('message_end', (event, context) => {
    if (!rootSession) {
      return;
    }
    observer.end(event, context);
  });

  pi.on('agent_end', () => {
    if (!rootSession) {
      return;
    }
    reporter.pause();
  });

  pi.on('session_shutdown', () => {
    if (!rootSession) {
      return;
    }
    if (displayRefreshTimer) {
      clearTimeout(displayRefreshTimer);
      displayRefreshTimer = undefined;
    }
    rootSession = false;
    observer.seen.clear();
    observer.activeMessageKey = undefined;
    return reporter.close();
  });
}

export function createOmpMetadataPublisher(pi) {
  return new HerdrMetadataPublisher({
    agent: 'omp',
    appliesToSource: 'herdr:omp',
    onError: (error, request) => {
      const detail = error instanceof Error ? error.message : String(error);
      const message = `[herdr-tps] metadata publish failed (${request?.params?.source}): ${detail}`;
      pi?.logger?.debug?.(message);
      if (process.env.HERDR_TPS_DEBUG === '1') {
        console.error(message);
      }
    },
  });
}

export default function herdrTpsExtension(pi) {
  const publisher = createOmpMetadataPublisher(pi);
  if (!publisher.enabled) {
    return;
  }

  registerOmpTpsHandlers(pi, () => new LiveTpsReporter({ publisher }), { requireUi: true });
}
