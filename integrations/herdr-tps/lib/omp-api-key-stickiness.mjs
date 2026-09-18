import { ompApiKeySelectionEntryType as ENTRY_TYPE } from './omp-api-key-observer.mjs';
const DEFAULT_PROVIDERS = ['opencode-go'];
const PATCH_STATE = Symbol.for('herdr.ompApiKeyStickiness');

function pinKey(provider, sessionId) {
  return `${provider}\0${sessionId}`;
}

function debug(pi, message) {
  pi?.logger?.debug?.(`[herdr-tps] ${message}`);
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
    const key = pinKey(data.provider, data.sessionId);
    if (data.action === 'pin' && (typeof data.credentialId === 'number' || typeof data.credentialId === 'string')) {
      state.pins.set(key, data.credentialId);
    } else if (data.action === 'release') {
      state.pins.delete(key);
    }
  }
}

function releasePin(state, provider, sessionId) {
  if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) return false;
  const key = pinKey(provider, sessionId);
  return state.pins.delete(key);
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

function isBlocked(auth, credentialId) {
  try {
    return (auth.listCredentialBlocks?.([credentialId])?.length ?? 0) > 0;
  } catch {
    // If block state cannot be read, do not bypass OMP's selector.
    return true;
  }
}

function credentialKey(credential) {
  const value = credential?.credential?.key;
  return typeof value === 'string' && value ? value : undefined;
}

function pinnedApiKey(state, provider, sessionId) {
  const key = pinKey(provider, sessionId);
  const credentialId = state.pins.get(key);
  if (credentialId === undefined) return undefined;
  const match = snapshotCredentials(state.auth, provider).find((item) => item.id === credentialId);
  const value = credentialKey(match);
  if (!value || isBlocked(state.auth, credentialId)) {
    releasePin(state, provider, sessionId);
    return undefined;
  }
  return value;
}

function rememberSelectedKey(state, provider, sessionId, apiKey) {
  if (typeof apiKey !== 'string' || !apiKey) return;
  const matches = snapshotCredentials(state.auth, provider).filter(
    (item) => credentialKey(item) === apiKey && !isBlocked(state.auth, item.id),
  );
  if (matches.length !== 1) return;
  const credentialId = matches[0].id;
  const key = pinKey(provider, sessionId);
  if (state.pins.get(key) === credentialId) return;
  state.pins.set(key, credentialId);
}

function wrapAuthStorage(auth, state) {
  state.auth = auth;
  if (auth[PATCH_STATE]) {
    auth[PATCH_STATE].pi = state.pi;
    auth[PATCH_STATE].providers = state.providers;
    return auth[PATCH_STATE];
  }

  const originalGetApiKey = auth.getApiKey.bind(auth);
  state.originalGetApiKey = originalGetApiKey;
  auth.getApiKey = async function getApiKeyWithSessionPin(provider, sessionId, ...rest) {
    if (!state.providers.has(provider) || typeof sessionId !== 'string' || !sessionId) {
      return originalGetApiKey(provider, sessionId, ...rest);
    }

    const existing = pinnedApiKey(state, provider, sessionId);
    if (existing !== undefined) return existing;

    const key = pinKey(provider, sessionId);
    const pending = state.pending.get(key);
    if (pending) return pending;
    const selection = (async () => {
      const selected = await originalGetApiKey(provider, sessionId, ...rest);
      rememberSelectedKey(state, provider, sessionId, selected);
      return selected;
    })();
    state.pending.set(key, selection);
    try {
      return await selection;
    } finally {
      if (state.pending.get(key) === selection) state.pending.delete(key);
    }
  };

  if (typeof auth.releaseSessionCredentialForReselection === 'function') {
    const originalRelease = auth.releaseSessionCredentialForReselection.bind(auth);
    auth.releaseSessionCredentialForReselection = function releaseSessionCredential(provider, sessionId, ...rest) {
      const released = originalRelease(provider, sessionId, ...rest);
      if (released) releasePin(state, provider, sessionId);
      return released;
    };
  }

  if (typeof auth.markUsageLimitReached === 'function') {
    const originalMark = auth.markUsageLimitReached.bind(auth);
    auth.markUsageLimitReached = async function markUsageLimit(provider, sessionId, ...rest) {
      try {
        return await originalMark(provider, sessionId, ...rest);
      } finally {
        releasePin(state, provider, sessionId);
      }
    };
  }

  if (typeof auth.rotateSessionCredential === 'function') {
    const originalRotate = auth.rotateSessionCredential.bind(auth);
    auth.rotateSessionCredential = async function rotateSessionCredential(provider, sessionId, ...rest) {
      const rotated = await originalRotate(provider, sessionId, ...rest);
      if (rotated) releasePin(state, provider, sessionId);
      return rotated;
    };
  }

  Object.defineProperty(auth, PATCH_STATE, { value: state, configurable: false });
  return state;
}

export function registerOmpApiKeyStickiness(pi, { providers = DEFAULT_PROVIDERS } = {}) {
  const state = {
    pi,
    providers: new Set(providers),
    pins: new Map(),
    pending: new Map(),
    auth: undefined,
  };

  const activate = (_event, context) => {
    const auth = context?.modelRegistry?.authStorage;
    if (!auth || typeof auth.getApiKey !== 'function') {
      debug(pi, 'OMP auth storage is unavailable; API-key stickiness was not installed');
      return;
    }
    const activeState = wrapAuthStorage(auth, state);
    loadEntries(activeState, context);
  };

  pi.on('session_start', activate);
  pi.on('session_switch', activate);
  return state;
}

export const ompApiKeyStickyEntryType = ENTRY_TYPE;
