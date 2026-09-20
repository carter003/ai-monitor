const ENTRY_TYPE = 'herdr-antigravity-route-v1';
const PROVIDER = 'google-antigravity';
const FALLBACK_PROVIDER = 'opencode-go';
const FALLBACK_MODEL = 'deepseek-v4.1-flash';
const DEFAULT_RESERVE_FRACTION = 0.15;
const PATCH_STATE = Symbol.for('herdr.ompAntigravitySessionRouter');

function debug(pi, message) {
  pi?.logger?.debug?.(`[herdr-tps] ${message}`);
}

function sessionId(context) {
  try {
    return context?.sessionManager?.getSessionId?.();
  } catch {
    return undefined;
  }
}

function entries(context) {
  try {
    const value = context?.sessionManager?.getEntries?.();
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

function hasConversation(context) {
  return entries(context).some(
    (entry) =>
      entry?.type === 'message' &&
      (entry.message?.role === 'user' || entry.message?.role === 'assistant'),
  );
}

function hasRouteEntry(context, id) {
  return entries(context).some(
    (entry) =>
      entry?.type === 'custom' &&
      entry.customType === ENTRY_TYPE &&
      entry.data?.sessionId === id,
  );
}

function isAntigravityGemini(model) {
  return model?.provider === PROVIDER && /^gemini(?:[-/.]|$)/i.test(String(model?.id ?? ''));
}

function appendRoute(state, data) {
  try {
    state.pi?.appendEntry?.(ENTRY_TYPE, data);
  } catch (error) {
    debug(state.pi, `cannot persist Antigravity session route: ${error?.message ?? error}`);
  }
}

function activeBlocks(auth, credentialId) {
  try {
    return auth.listCredentialBlocks?.([credentialId]) ?? [];
  } catch {
    return [{ credentialId }];
  }
}

function accountMatchesReport(account, report) {
  if (report?.credentialId !== undefined) return report.credentialId === account.credentialId;
  const metadata = report?.metadata ?? {};
  const compared = ['accountId', 'email', 'projectId'].filter(
    (field) => metadata[field] !== undefined && account[field] !== undefined,
  );
  return compared.length > 0 && compared.every((field) => metadata[field] === account[field]);
}

function geminiFiveHourRemaining(report) {
  const limits = Array.isArray(report?.limits) ? report.limits : [];
  const candidates = limits.filter((limit) => {
    const windowId = String(limit?.scope?.windowId ?? limit?.window?.id ?? '').toLowerCase();
    const id = String(limit?.id ?? '').toLowerCase();
    const label = String(limit?.label ?? '').toLowerCase();
    return windowId === '5h' && (id.includes(':gemini-5h') || label === 'gemini');
  });
  const values = candidates
    .map((limit) => Number(limit?.amount?.remainingFraction))
    .filter((value) => Number.isFinite(value) && value >= 0);
  return values.length > 0 ? Math.min(...values) : undefined;
}

function rankedAccounts(auth, session, reports) {
  const accounts = auth
    .listOAuthAccounts(PROVIDER, session)
    .filter((account) => activeBlocks(auth, account.credentialId).length === 0);
  const providerReports = Array.isArray(reports)
    ? reports.filter((report) => report?.provider === PROVIDER)
    : [];
  const ranked = [];
  let hasUnknown = false;
  for (const account of accounts) {
    const matches = providerReports.filter((report) => accountMatchesReport(account, report));
    const values = matches
      .map(geminiFiveHourRemaining)
      .filter((value) => value !== undefined);
    if (values.length === 0) {
      hasUnknown = true;
      continue;
    }
    ranked.push({ account, remainingFraction: Math.min(...values) });
  }
  ranked.sort((left, right) => right.remainingFraction - left.remainingFraction);
  return { ranked, hasUnknown: hasUnknown || ranked.length !== accounts.length };
}

function installUsagePreflightGuard(auth, state) {
  state.auth = auth;
  if (auth[PATCH_STATE]) {
    const active = auth[PATCH_STATE];
    active.pi = state.pi;
    return active;
  }
  const original = auth.getModelUsageHealth?.bind(auth);
  if (typeof original !== 'function') return state;
  state.originalGetModelUsageHealth = original;
  auth.getModelUsageHealth = async function getModelUsageHealthOnce(provider, options, ...rest) {
    const id = options?.sessionId;
    if (typeof id === 'string' && state.settledSessions.has(id)) {
      return { state: 'healthy', accounts: [] };
    }
    return original(provider, options, ...rest);
  };
  Object.defineProperty(auth, PATCH_STATE, { value: state, configurable: false });
  return state;
}

function selectSessionAccount(state, id) {
  const selected = state.selections.get(id);
  if (selected) return selected;

  const selection = (async () => {
    try {
      const reports = await state.auth.fetchUsageReports?.();
      const { ranked, hasUnknown } = rankedAccounts(state.auth, id, reports);
      const best = ranked[0];
      if (!best || hasUnknown) return { best, hasUnknown, pinned: false };
      const pinned = state.auth.pinSessionOAuthAccount(
        PROVIDER,
        id,
        best.account.credentialId,
      );
      return { best, hasUnknown: false, pinned };
    } catch (error) {
      debug(state.pi,         `Antigravity session account selection failed open: ${error?.message ?? error}`);
      return { best: undefined, hasUnknown: true, pinned: false, error };
    }
  })();
  state.selections.set(id, selection);
  return selection;
}

async function routeFirstTurn(state, context) {
  const id = sessionId(context);
  if (!id || state.settledSessions.has(id)) return;

  const model = context?.model;
  const base = {
    sessionId: id,
    sourceProvider: model?.provider,
    sourceModel: model?.id,
    at: Date.now(),
  };
  try {
    if (!isAntigravityGemini(model)) return;

    const { best, hasUnknown, pinned, error } = await selectSessionAccount(state, id);
    if (error) {
      appendRoute(state, { ...base, action: 'keep-antigravity', reason: 'routing-error' });
      return;
    }
    if (!best || hasUnknown) {
      appendRoute(state, { ...base, action: 'keep-antigravity', reason: 'usage-unknown' });
      return;
    }

    if (best.remainingFraction < state.reserveFraction) {
      const fallback = context.modelRegistry?.find?.(FALLBACK_PROVIDER, FALLBACK_MODEL);
      if (!fallback) {
        appendRoute(state, {
          ...base,
          action: 'keep-antigravity',
          reason: 'fallback-unavailable',
          remainingFraction: best.remainingFraction,
        });
        return;
      }
      const changed = await state.pi.setModel(fallback);
      if (changed === false) {
        appendRoute(state, {
          ...base,
          action: 'keep-antigravity',
          reason: 'fallback-rejected',
          remainingFraction: best.remainingFraction,
        });
        return;
      }
      state.pi.setThinkingLevel?.('high');
      appendRoute(state, {
        ...base,
        action: 'fallback',
        targetProvider: FALLBACK_PROVIDER,
        targetModel: FALLBACK_MODEL,
        remainingFraction: best.remainingFraction,
      });
      return;
    }

    appendRoute(state, {
      ...base,
      action: pinned ? 'pin-antigravity' : 'keep-antigravity',
      reason: pinned ? undefined : 'pin-rejected',
      credentialId: pinned ? best.account.credentialId : undefined,
      remainingFraction: best.remainingFraction,
    });
  } finally {
    // From this point onward OMP's per-request usage preflight is suppressed
    // for this exact session. Hard-error/429 recovery uses a separate path.
    state.settledSessions.add(id);
  }
}

export function registerOmpAntigravitySessionRouter(
  pi,
  { reserveFraction = DEFAULT_RESERVE_FRACTION } = {},
) {
  const state = {
    pi,
    reserveFraction,
    settledSessions: new Set(),
    selections: new Map(),
    auth: undefined,
  };
  let activeState = state;

  const activate = (_event, context) => {
    const auth = context?.modelRegistry?.authStorage;
    if (!auth) return {};
    const active = installUsagePreflightGuard(auth, state);
    activeState = active;
    const id = sessionId(context);
    if (id && (hasConversation(context) || hasRouteEntry(context, id))) {
      active.settledSessions.add(id);
    }
    return { active, id };
  };

  pi.on('session_start', async (event, context) => {
    const { active, id } = activate(event, context);
    if (active && id && !active.settledSessions.has(id)) {
      // Title generation starts before before_agent_start. Pre-pin the chosen
      // account now so OMP's title child session inherits the same credential.
      await selectSessionAccount(active, id);
    }
  });
  pi.on('session_switch', activate);
  pi.on('session_branch', activate);
  pi.on('session_tree', activate);
  pi.on('before_agent_start', (_event, context) => routeFirstTurn(activeState, context));
  return state;
}

export const ompAntigravityRouteEntryType = ENTRY_TYPE;
