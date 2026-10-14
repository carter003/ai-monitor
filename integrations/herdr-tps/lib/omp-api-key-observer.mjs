// OpenCode Go session router and observer. OMP 18.10.4 records API-key affinity
// but does not read it during selection (only the OAuth path does), so every
// request is ranked again. This wrapper keeps the first resolved credential for
// the session until reset, release, store replacement, or an explicit
// limit/rotation path invalidates it.

const ENTRY_TYPE = 'herdr-api-key-sticky-v1';
const OBSERVER_STATE = Symbol.for('herdr.ompApiKeyObserver');
const DEFAULT_PROVIDERS = ['opencode-go'];
// Sessions without an extension scope (for example background workers) have no
// shutdown event. Bound their caches while preserving all live scoped sessions.
const MAX_UNMANAGED_SESSIONS = 128;

function selectionKey(provider, sessionId) {
  return `${provider}\0${sessionId}`;
}

function debug(pi, message) {
  pi?.logger?.debug?.(`[herdr-tps] ${message}`);
}

function forgetSession(state, sessionId) {
  for (const cache of [state.selections, state.resolved, state.inflight, state.pendingReasons]) {
    for (const key of cache.keys()) {
      if (key.slice(key.indexOf('\0') + 1) === sessionId) cache.delete(key);
    }
  }
  state.unmanagedSessions.delete(sessionId);
  state.sessionEpochs.delete(sessionId);
}

function touchSession(state, sessionId) {
  if (!state.sessionEpochs.has(sessionId)) state.sessionEpochs.set(sessionId, {});
  state.unmanagedSessions.delete(sessionId);
  if (state.writers.has(sessionId)) return;
  state.unmanagedSessions.set(sessionId, true);
  while (state.unmanagedSessions.size > MAX_UNMANAGED_SESSIONS) {
    forgetSession(state, state.unmanagedSessions.keys().next().value);
  }
}

function appendState(state, data) {
  const writer = state.writers.get(data.sessionId);
  if (!writer) return;
  try {
    if (writer.sessionManager.getSessionId() !== data.sessionId) return;
    writer.pi.appendEntry?.(ENTRY_TYPE, data);
  } catch (error) {
    debug(writer.pi, `cannot persist API-key selection: ${error?.message ?? error}`);
  }
}

function loadEntries(state, context) {
  const sessionId = context?.sessionManager?.getSessionId?.();
  let entries;
  try {
    entries = context?.sessionManager?.getEntries?.();
  } catch {
    return;
  }
  if (!Array.isArray(entries)) return;

  for (const entry of entries) {
    if (entry?.type !== 'custom' || entry.customType !== ENTRY_TYPE) continue;
    const data = entry.data;
    if (
      !data ||
      typeof data.provider !== 'string' ||
      data.sessionId !== sessionId ||
      !state.providers.has(data.provider)
    ) {
      continue;
    }
    const key = selectionKey(data.provider, data.sessionId);
    if (
      data.action === 'pin' &&
      (typeof data.credentialId === 'number' || typeof data.credentialId === 'string')
    ) {
      if (state.selections.get(key) !== data.credentialId) state.resolved.delete(key);
      state.selections.set(key, data.credentialId);
      state.pendingReasons.delete(key);
    } else if (data.action === 'release') {
      const previousCredentialId = state.selections.get(key);
      state.selections.delete(key);
      state.resolved.delete(key);
      state.pendingReasons.set(key, {
        reason: typeof data.reason === 'string' ? data.reason : 'reselection',
        previousCredentialId,
      });
    }
  }
}

function credentialId(value) {
  return typeof value === 'number' || typeof value === 'string' ? value : undefined;
}

function activeBlock(state, provider, selectedCredentialId) {
  if (typeof selectedCredentialId !== 'number') return undefined;
  try {
    return state.auth?.blocks
      ?.list?.([selectedCredentialId])
      ?.find?.((entry) =>
        entry?.providerKey === `${provider}:api_key` &&
        typeof entry.blockedUntilMs === 'number' &&
        entry.blockedUntilMs > Date.now());
  } catch {
    return undefined;
  }
}

function restoredSelection(state, provider, selectedCredentialId) {
  if (typeof selectedCredentialId !== 'number') return undefined;
  try {
    const stored = state.auth?.credentials
      ?.list?.(provider)
      ?.find?.((entry) => entry?.id === selectedCredentialId);
    const apiKey = stored?.disabledCause == null && stored?.credential?.type === 'api_key'
      ? stored.credential.key
      : undefined;
    // OMP's private config resolver owns commands and environment references.
    // They are expressions, not bearer keys; let native resolution handle them.
    if (typeof apiKey === 'string' && (apiKey.startsWith('!') || process.env[apiKey])) return undefined;
    return typeof apiKey === 'string' && apiKey ? { apiKey, credentialId: selectedCredentialId } : undefined;
  } catch {
    return undefined;
  }
}

function hasHigherPriorityLogin(state, provider, selectedCredentialId) {
  try {
    const rows = state.auth?.credentials?.list?.(provider) ?? [];
    const selected = rows.find((row) => row.id === selectedCredentialId)?.credential;
    if (selected?.type !== 'api_key' || selected.source === 'login') return false;
    return rows.some(({ credential, disabledCause }) =>
      disabledCause == null && credential?.type === 'api_key' && credential.source === 'login' &&
      typeof credential.key === 'string' && credential.key.length > 0 &&
      !state.auth.keys.isKeylessFallback?.(provider, credential));
  } catch {
    return false;
  }
}


function observeSelection(state, provider, sessionId, selectedCredentialId) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return;
  const id = credentialId(selectedCredentialId);
  if (id === undefined) return;

  const key = selectionKey(provider, sessionId);
  const previousCredentialId = state.selections.get(key);
  if (previousCredentialId === id) return;
  state.sessionEpochs.set(sessionId, {});
  const pending = state.pendingReasons.get(key);
  state.pendingReasons.delete(key);
  const priorCredentialId = previousCredentialId ?? pending?.previousCredentialId;
  const blockedUntilMs = activeBlock(state, provider, previousCredentialId)?.blockedUntilMs;
  const reason = pending?.reason ?? (
    priorCredentialId === undefined ? 'initial' : blockedUntilMs ? 'blocked' : 'usage-ranking'
  );
  state.selections.set(key, id);
  appendState(state, {
    action: 'pin',
    provider,
    sessionId,
    credentialId: id,
    ...(priorCredentialId === undefined ? {} : { previousCredentialId: priorCredentialId }),
    reason,
    ...(blockedUntilMs === undefined ? {} : { blockedUntilMs }),
    at: Date.now(),
  });
}


function observeRelease(state, provider, sessionId, reason, cancelInflight = true) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return;
  const key = selectionKey(provider, sessionId);
  const previousCredentialId = state.selections.get(key);
  if (state.sessionEpochs.has(sessionId)) state.sessionEpochs.set(sessionId, {});
  state.resolved.delete(key);
  if (cancelInflight) state.inflight.delete(key);
  if (!state.selections.delete(key)) return;
  state.pendingReasons.set(key, { reason, previousCredentialId });
  appendState(state, { action: 'release', provider, sessionId, reason, at: Date.now() });
}

function releaseSession(state, sessionId, reason) {
  if (typeof sessionId !== 'string' || !sessionId) return;
  for (const provider of state.providers) {
    const key = selectionKey(provider, sessionId);
    state.releaseReasons.set(key, reason);
    try {
      state.auth?.sessions?.release?.(provider, sessionId);
    } finally {
      state.releaseReasons.delete(key);
    }
    // A pin may exist only in the local extension state (for example after a
    // store reload), so make cleanup idempotent even when native release says false.
    observeRelease(state, provider, sessionId, reason);
  }
}

function wrapResetBoundary(sessionManager, state) {
  if (typeof sessionManager?.appendResetBoundary !== 'function') return;
  if (state.sessionManagers.has(sessionManager)) return;
  state.sessionManagers.add(sessionManager);
  const originalReset = sessionManager.appendResetBoundary.bind(sessionManager);
  sessionManager.appendResetBoundary = function appendResetBoundaryWithCredentialRelease(...args) {
    releaseSession(state, sessionManager.getSessionId?.(), 'reset');
    return originalReset(...args);
  };
}

function wrapNamespaces(auth, state) {
  const { keys, sessions, limits } = auth;
  const previous = state.namespaces;
  if (previous?.keys === keys && previous.sessions === sessions && previous.limits === limits) {
    return;
  }
  const namespaces = { keys, sessions, limits };
  state.namespaces = namespaces;
  if (previous) {
    // Credential IDs belong to a store; even an identical ID must be pinned
    // again after replacement. Persist releases so collectors drop old pins.
    for (const key of state.selections.keys()) {
      const separator = key.indexOf('\0');
      observeRelease(state, key.slice(0, separator), key.slice(separator + 1), 'store-replaced');
    }
    state.selections.clear();
    state.resolved.clear();
    state.inflight.clear();
  }

  const originalGetWithCredential = keys.getWithCredential.bind(keys);
  keys.getWithCredential = async function getWithCredentialWithObservation(
    provider,
    sessionId,
    ...rest
  ) {
    if (
      state.namespaces !== namespaces || !state.writers.size ||
      !state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId ||
      rest[0]?.recordAffinity === false
    ) {
      return originalGetWithCredential(provider, sessionId, ...rest);
    }
    const key = selectionKey(provider, sessionId);
    touchSession(state, sessionId);
    const running = state.inflight.get(key);
    if (running) return running;
    const nativeReason = () => {
      if (sessions.isRestricted?.(provider, sessionId)) return 'restricted';
      // source() is public and does not resolve a key. Runtime/config/env and
      // OAuth precedence must win over both live secrets and durable pins.
      const source = keys.source?.(provider, { env: 'aliases' });
      return source && source.kind !== 'api_key' ? 'auth-source-changed' : undefined;
    };
    const isCurrent = () => state.namespaces === namespaces && state.inflight.get(key) === selection;
    const selection = (async () => {
      try {
        // Like the native cascade, adopt other processes' store writes before
        // reading credentials. Unchanged local stores do only version checks.
        await auth.credentials?.poll?.();
      } catch {
        // Native polling also keeps the in-memory pool usable after a failure.
      }
      if (!isCurrent()) return originalGetWithCredential(provider, sessionId, ...rest);
      const reason = nativeReason();
      if (reason) {
        observeRelease(state, provider, sessionId, reason);
        return originalGetWithCredential(provider, sessionId, ...rest);
      }
      const selectedCredentialId = state.selections.get(key);
      if (selectedCredentialId !== undefined && hasHigherPriorityLogin(state, provider, selectedCredentialId)) {
        // source() groups both tiers as api_key; a deliberate /login still
        // outranks an existing non-login key, as it does in the native cascade.
        observeRelease(state, provider, sessionId, 'auth-source-changed', false);
      } else if (selectedCredentialId !== undefined && activeBlock(state, provider, selectedCredentialId)) {
        observeRelease(state, provider, sessionId, 'blocked', false);
      } else if (selectedCredentialId !== undefined) {
        const cached = state.resolved.get(key) ?? restoredSelection(state, provider, selectedCredentialId);
        if (cached) {
          state.resolved.set(key, cached);
          return cached;
        }
        observeRelease(state, provider, sessionId, 'credential-unavailable', false);
      }
      const generation = state.generation;
      const selected = await originalGetWithCredential(provider, sessionId, ...rest);
      const changedReason = nativeReason();
      if (changedReason) {
        if (isCurrent()) observeRelease(state, provider, sessionId, changedReason);
        return originalGetWithCredential(provider, sessionId, ...rest);
      }
      if (isCurrent() && state.generation === generation) {
        observeSelection(state, provider, sessionId, selected?.credentialId);
        if (credentialId(selected?.credentialId) !== undefined && typeof selected?.apiKey === 'string') {
          state.resolved.set(key, selected);
        }
      }
      return selected;
    })();
    state.inflight.set(key, selection);
    try {
      return await selection;
    } finally {
      if (state.inflight.get(key) === selection) state.inflight.delete(key);
    }
  };

  const originalRelease = sessions.release.bind(sessions);
  sessions.release = function releaseSessionCredential(provider, sessionId) {
    const released = originalRelease(provider, sessionId);
    if (state.namespaces === namespaces) {
      const reason = state.releaseReasons.get(selectionKey(provider, sessionId)) ?? 'reselection';
      observeRelease(state, provider, sessionId, reason);
    }
    return released;
  };

  const originalMark = limits.markReached.bind(limits);
  limits.markReached = async function markReached(provider, sessionId, ...rest) {
    const epoch = state.sessionEpochs.get(sessionId);
    try {
      return await originalMark(provider, sessionId, ...rest);
    } finally {
      if (state.namespaces === namespaces && state.sessionEpochs.get(sessionId) === epoch) {
        observeRelease(state, provider, sessionId, 'usage-limit');
      }
    }
  };

  const originalRotate = limits.rotate.bind(limits);
  limits.rotate = async function rotate(provider, sessionId, ...rest) {
    const epoch = state.sessionEpochs.get(sessionId);
    // OMP 18.4.0 returns a CredentialRotation object, which is truthy even when
    // no sibling was available, so only an actual switch releases the pin.
    const rotated = await originalRotate(provider, sessionId, ...rest);
    if (rotated?.switched && state.namespaces === namespaces && state.sessionEpochs.get(sessionId) === epoch) {
      observeRelease(state, provider, sessionId, 'rotation');
    }
    return rotated;
  };
}

function wrapAuthStorage(auth, state) {
  state.auth = auth;
  if (auth[OBSERVER_STATE]) {
    const activeState = auth[OBSERVER_STATE];
    activeState.providers = state.providers;
    wrapNamespaces(auth, activeState);
    return activeState;
  }

  wrapNamespaces(auth, state);
  auth.credentials?.onGeneration?.(() => {
    state.resolved.clear();
    state.generation += 1;
    wrapNamespaces(auth, state);
  });
  Object.defineProperty(auth, OBSERVER_STATE, { value: state, configurable: false });
  return state;
}


export function registerOmpApiKeyObserver(pi, { providers = DEFAULT_PROVIDERS } = {}) {
  const state = {
    providers: new Set(providers),
    selections: new Map(),
    resolved: new Map(),
    inflight: new Map(),
    pendingReasons: new Map(),
    releaseReasons: new Map(),
    writers: new Map(),
    unmanagedSessions: new Map(),
    sessionEpochs: new Map(),
    generation: 0,
    sessionManagers: new WeakSet(),
    auth: undefined,
  };
  let writerState;
  let writerSessionId;
  let writer;
  const detachWriter = () => {
    if (writerState && writerState.writers.get(writerSessionId) === writer) {
      writerState.writers.delete(writerSessionId);
      forgetSession(writerState, writerSessionId);
      if (!writerState.writers.size) {
        for (const sessionId of writerState.unmanagedSessions.keys()) forgetSession(writerState, sessionId);
      }
    }
    writerState = undefined;
    writerSessionId = undefined;
    writer = undefined;
  };
  const activate = (_event, context) => {
    const auth = context?.modelRegistry?.authStorage;
    const manager = context?.sessionManager;
    const sessionId = manager?.getSessionId?.();
    if (
      typeof auth?.keys?.getWithCredential !== 'function' ||
      typeof auth.sessions?.release !== 'function' ||
      typeof auth.limits?.markReached !== 'function' ||
      typeof auth.limits?.rotate !== 'function' ||
      typeof sessionId !== 'string' || !sessionId
    ) {
      detachWriter();
      debug(
        pi,
        'OMP auth/session namespaces are unavailable; API-key observation was not installed',
      );
      return;
    }
    const activeState = wrapAuthStorage(auth, state);
    if (
      writerState !== activeState ||
      writerSessionId !== sessionId ||
      writer?.sessionManager !== manager
    ) {
      detachWriter();
      writer = { pi, sessionManager: manager };
      // A new scope can revive a session while its previous scope is still
      // unwinding. Cancel the old scope's work without discarding durable pins.
      for (const provider of activeState.providers) {
        activeState.inflight.delete(selectionKey(provider, sessionId));
      }
      activeState.writers.set(sessionId, writer);
      activeState.sessionEpochs.set(sessionId, {});
      activeState.unmanagedSessions.delete(sessionId);
      writerState = activeState;
      writerSessionId = sessionId;
    }
    loadEntries(activeState, context);
    wrapResetBoundary(manager, activeState);
  };
  pi.on('session_start', activate);
  pi.on('session_switch', activate);
  pi.on('session_branch', activate);
  pi.on('session_shutdown', detachWriter);
  return state;
}

// Keep the on-disk entry type stable so existing sessions and collector
// backfills remain compatible after separating observation from routing policy.
export const ompApiKeySelectionEntryType = ENTRY_TYPE;
