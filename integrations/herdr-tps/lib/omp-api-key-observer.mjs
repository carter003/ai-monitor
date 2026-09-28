// OpenCode Go session router and observer. OMP 18.4.2 records API-key affinity
// but does not read it during selection, so every request is ranked again. This
// wrapper keeps the first resolved credential for the session until reset,
// release, store replacement, or an explicit limit/rotation path invalidates it.
const ENTRY_TYPE = 'herdr-api-key-sticky-v1';
const OBSERVER_STATE = Symbol.for('herdr.ompApiKeyObserver');
const DEFAULT_PROVIDERS = ['opencode-go'];

function selectionKey(provider, sessionId) {
  return `${provider}\0${sessionId}`;
}

function debug(pi, message) {
  pi?.logger?.debug?.(`[herdr-tps] ${message}`);
}

function appendState(state, data) {
  try {
    state.pi?.appendEntry?.(ENTRY_TYPE, data);
  } catch (error) {
    debug(state.pi, `cannot persist API-key selection: ${error?.message ?? error}`);
  }
}

function loadEntries(state, context) {
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
      typeof data.sessionId !== 'string' ||
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
    return typeof apiKey === 'string' && apiKey ? { apiKey, credentialId: selectedCredentialId } : undefined;
  } catch {
    return undefined;
  }
}


function observeSelection(state, provider, sessionId, selectedCredentialId) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return;
  const id = credentialId(selectedCredentialId);
  if (id === undefined) return;

  const key = selectionKey(provider, sessionId);
  const previousCredentialId = state.selections.get(key);
  if (previousCredentialId === id) return;
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


function observeRelease(state, provider, sessionId, reason) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return;
  const key = selectionKey(provider, sessionId);
  const previousCredentialId = state.selections.get(key);
  state.resolved.delete(key);
  state.inflight.delete(key);
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
    if (state.selections.has(key)) {
      observeRelease(state, provider, sessionId, reason);
    }
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
    if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) {
      return originalGetWithCredential(provider, sessionId, ...rest);
    }
    const key = selectionKey(provider, sessionId);
    const selectedCredentialId = state.selections.get(key);
    if (selectedCredentialId !== undefined && activeBlock(state, provider, selectedCredentialId)) {
      observeRelease(state, provider, sessionId, 'blocked');
    } else if (selectedCredentialId !== undefined) {
      const cached = state.resolved.get(key) ?? restoredSelection(state, provider, selectedCredentialId);
      if (cached) {
        state.resolved.set(key, cached);
        return cached;
      }
      observeRelease(state, provider, sessionId, 'credential-unavailable');
    }

    const running = state.inflight.get(key);
    if (running) return running;
    const selection = (async () => {
      const selected = await originalGetWithCredential(provider, sessionId, ...rest);
      if (state.namespaces === namespaces) {
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
    if (released && state.namespaces === namespaces) {
      const reason = state.releaseReasons.get(selectionKey(provider, sessionId)) ?? 'reselection';
      observeRelease(state, provider, sessionId, reason);
    }
    return released;
  };

  const originalMark = limits.markReached.bind(limits);
  limits.markReached = async function markReached(provider, sessionId, ...rest) {
    try {
      return await originalMark(provider, sessionId, ...rest);
    } finally {
      if (state.namespaces === namespaces) {
        observeRelease(state, provider, sessionId, 'usage-limit');
      }
    }
  };

  const originalRotate = limits.rotate.bind(limits);
  limits.rotate = async function rotate(provider, sessionId, ...rest) {
    // OMP 18.4.0 returns a CredentialRotation object, which is truthy even when
    // no sibling was available, so only an actual switch releases the pin.
    const rotated = await originalRotate(provider, sessionId, ...rest);
    if (rotated?.switched && state.namespaces === namespaces) {
      observeRelease(state, provider, sessionId, 'rotation');
    }
    return rotated;
  };
}

function wrapAuthStorage(auth, state) {
  state.auth = auth;
  if (auth[OBSERVER_STATE]) {
    const activeState = auth[OBSERVER_STATE];
    activeState.pi = state.pi;
    activeState.providers = state.providers;
    wrapNamespaces(auth, activeState);
    return activeState;
  }

  wrapNamespaces(auth, state);
  // OMP carries generation subscribers to the new store and notifies them
  // synchronously after replacing its namespaces. Ordinary reloads keep the
  // same objects and must not reset selections or stack wrappers.
  auth.credentials?.onGeneration?.(() => {
    state.resolved.clear();
    wrapNamespaces(auth, state);
  });
  Object.defineProperty(auth, OBSERVER_STATE, { value: state, configurable: false });
  return state;
}


export function registerOmpApiKeyObserver(pi, { providers = DEFAULT_PROVIDERS } = {}) {
  const state = {
    pi,
    providers: new Set(providers),
    selections: new Map(),
    resolved: new Map(),
    inflight: new Map(),
    pendingReasons: new Map(),
    releaseReasons: new Map(),
    sessionManagers: new WeakSet(),
    auth: undefined,
  };
  const activate = (_event, context) => {
    const auth = context?.modelRegistry?.authStorage;
    if (
      typeof auth?.keys?.getWithCredential !== 'function' ||
      typeof auth.sessions?.release !== 'function' ||
      typeof auth.limits?.markReached !== 'function' ||
      typeof auth.limits?.rotate !== 'function'
    ) {
      debug(pi, 'OMP auth namespaces are unavailable; API-key observation was not installed');
      return;
    }
    const activeState = wrapAuthStorage(auth, state);
    loadEntries(activeState, context);
    wrapResetBoundary(context?.sessionManager, activeState);
  };
  pi.on('session_start', activate);
  pi.on('session_switch', activate);
  return state;
}

// Keep the on-disk entry type stable so existing sessions and collector
// backfills remain compatible after separating observation from routing policy.
export const ompApiKeySelectionEntryType = ENTRY_TYPE;
