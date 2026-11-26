export function registerOmpSubagentModelPolicy(pi, { agentModelOverrides, getUpstreamPin, agentDefinitions = [] }) {
  pi.on('before_subagent_spawn', (event, context) => {
    // Literal selectors and user-tagged model agents have no role to replace.
    if (!event.modelRole) return;
    if (event.invocationKind === 'eval') {
      const definition = agentDefinitions.find(agent => agent.name === event.agent);
      // Unknown agents and roles different from the definition are per-call choices.
      const selectors = Array.isArray(definition?.model) ? definition.model : [definition?.model];
      if (selectors.length !== 1 || selectors[0] !== `@${event.modelRole}`) return;
    }
    const settings = pi?.pi?.settings;
    const overrides = agentModelOverrides?.get?.(settings);
    const override = overrides?.[event.agent];
    if (
      (Array.isArray(override) ? override.some((selector) => selector.trim()) : override?.trim()) ||
      settings?.getModelRole?.(event.modelRole)?.trim()
    ) return;

    // An unconfigured @slow/@smol/@task is an OMP default, not user consent
    // to leave the active model. Read at dispatch, never cache across sessions.
    const model = context?.models?.current?.();
    if (!model) {
      return { block: true, reason: 'Cannot inherit the parent model: no active model.' };
    }
    const upstream = getUpstreamPin?.(model);
    return {
      model: `${model.provider}/${model.id}${upstream ? `@${upstream}` : ''}`,
      note: 'Inherits parent active model; no explicit subagent model configured',
    };
  });
}

export default async function subagentModelExtension(pi) {
  try {
    const [{ lookup }, { getUpstreamPin }, { discoverAgents }] = await Promise.all([
      import('@oh-my-pi/pi-coding-agent/config/registry'),
      import('@oh-my-pi/pi-catalog/models'),
      import('@oh-my-pi/pi-coding-agent/task/discovery'),
    ]);
    const { agents } = await discoverAgents(process.cwd());
    registerOmpSubagentModelPolicy(pi, {
      agentModelOverrides: lookup('task.agentModelOverrides'), getUpstreamPin,
      agentDefinitions: agents,
    });
  } catch (error) {
    pi?.logger?.warn?.(`[herdr-tps] subagent policy unavailable: ${error.message}`);
  }
}
