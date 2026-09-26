// Passive OpenCode Go account observer. It records which credential OMP
// actually returned without changing selection, rotation or block behavior.
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
      state.selections.set(key, data.credentialId);
    } else if (data.action === 'release') {
      state.selections.delete(key);
    }
  }
}

function credentialId(value) {
  return typeof value === 'number' || typeof value === 'string' ? value : undefined;
}


function observeSelection(state, provider, sessionId, selectedCredentialId) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return;
  const id = credentialId(selectedCredentialId);
  if (id === undefined) return;

  const key = selectionKey(provider, sessionId);
  if (state.selections.get(key) === id) return;
  state.selections.set(key, id);
  appendState(state, { action: 'pin', provider, sessionId, credentialId: id, at: Date.now() });
}


function observeRelease(state, provider, sessionId, reason) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return;
  const key = selectionKey(provider, sessionId);
  if (!state.selections.delete(key)) return;
  appendState(state, { action: 'release', provider, sessionId, reason, at: Date.now() });
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
  }

  const originalGetWithCredential = keys.getWithCredential.bind(keys);
  keys.getWithCredential = async function getWithCredentialWithObservation(
    provider,
    sessionId,
    ...rest
  ) {
    const selected = await originalGetWithCredential(provider, sessionId, ...rest);
    if (state.namespaces === namespaces) {
      observeSelection(state, provider, sessionId, selected?.credentialId);
    }
    return selected;
  };

  const originalRelease = sessions.release.bind(sessions);
  sessions.release = function releaseSessionCredential(provider, sessionId) {
    const released = originalRelease(provider, sessionId);
    if (released && state.namespaces === namespaces) {
      observeRelease(state, provider, sessionId, 'reselection');
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
    const rotated = await originalRotate(provider, sessionId, ...rest);
    if (rotated && state.namespaces === namespaces) {
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
  auth.credentials?.onGeneration?.(() => wrapNamespaces(auth, state));
  Object.defineProperty(auth, OBSERVER_STATE, { value: state, configurable: false });
  return state;
}


export function registerOmpApiKeyObserver(pi, { providers = DEFAULT_PROVIDERS } = {}) {
  const state = {
    pi,
    providers: new Set(providers),
    selections: new Map(),
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
  };
  pi.on('session_start', activate);
  pi.on('session_switch', activate);
  return state;
}

// Keep the on-disk entry type stable so existing sessions and collector
// backfills remain compatible after separating observation from routing policy.
export const ompApiKeySelectionEntryType = ENTRY_TYPE;
