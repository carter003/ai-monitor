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

function snapshotCredentials(auth, provider) {
  try {
    const credentials = auth.exportSnapshot?.().credentials;
    return Array.isArray(credentials)
      ? credentials.filter(
          (item) =>
            item?.provider === provider &&
            item?.credential?.type === 'api_key' &&
            item?.credential?.source === 'login',
        )
      : [];
  } catch {
    return [];
  }
}

function observeSelection(state, provider, sessionId, apiKey) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return;
  if (typeof apiKey !== 'string' || !apiKey) return;
  const matches = snapshotCredentials(state.auth, provider).filter(
    (item) => item?.credential?.key === apiKey,
  );
  if (matches.length !== 1) return;

  const credentialId = matches[0].id;
  const key = selectionKey(provider, sessionId);
  if (state.selections.get(key) === credentialId) return;
  state.selections.set(key, credentialId);
  appendState(state, { action: 'pin', provider, sessionId, credentialId, at: Date.now() });
}

function observeRelease(state, provider, sessionId, reason) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return;
  const key = selectionKey(provider, sessionId);
  if (!state.selections.delete(key)) return;
  appendState(state, { action: 'release', provider, sessionId, reason, at: Date.now() });
}

function wrapAuthStorage(auth, state) {
  state.auth = auth;
  if (auth[OBSERVER_STATE]) {
    auth[OBSERVER_STATE].pi = state.pi;
    auth[OBSERVER_STATE].providers = state.providers;
    return auth[OBSERVER_STATE];
  }

  const originalGetApiKey = auth.getApiKey.bind(auth);
  auth.getApiKey = async function getApiKeyWithObservation(provider, sessionId, ...rest) {
    const selected = await originalGetApiKey(provider, sessionId, ...rest);
    observeSelection(state, provider, sessionId, selected);
    return selected;
  };

  if (typeof auth.releaseSessionCredentialForReselection === 'function') {
    const originalRelease = auth.releaseSessionCredentialForReselection.bind(auth);
    auth.releaseSessionCredentialForReselection = function releaseSessionCredential(
      provider,
      sessionId,
      ...rest
    ) {
      const released = originalRelease(provider, sessionId, ...rest);
      if (released) observeRelease(state, provider, sessionId, 'reselection');
      return released;
    };
  }

  if (typeof auth.markUsageLimitReached === 'function') {
    const originalMark = auth.markUsageLimitReached.bind(auth);
    auth.markUsageLimitReached = async function markUsageLimit(provider, sessionId, ...rest) {
      try {
        return await originalMark(provider, sessionId, ...rest);
      } finally {
        observeRelease(state, provider, sessionId, 'usage-limit');
      }
    };
  }

  if (typeof auth.rotateSessionCredential === 'function') {
    const originalRotate = auth.rotateSessionCredential.bind(auth);
    auth.rotateSessionCredential = async function rotateSessionCredential(
      provider,
      sessionId,
      ...rest
    ) {
      const rotated = await originalRotate(provider, sessionId, ...rest);
      if (rotated) observeRelease(state, provider, sessionId, 'rotation');
      return rotated;
    };
  }

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
    if (!auth || typeof auth.getApiKey !== 'function') {
      debug(pi, 'OMP auth storage is unavailable; API-key observation was not installed');
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
