import type { AgentMetadata, Profile } from '../types';

type AvailabilityFlag = Pick<AgentMetadata, 'available'>;

/** The `available` flag is optional on the wire — an older server omits it and
 *  custom agents never set it — so only an explicit `false` means the harness
 *  binary is missing from the host PATH. */
export function isAgentAvailable(agent: AvailabilityFlag): boolean {
  return agent.available !== false;
}

/** The agents whose harness is installed, in their original order. */
export function availableAgents<T extends AvailabilityFlag>(agents: T[]): T[] {
  return agents.filter(isAgentAvailable);
}

/** The `kind`s whose harness is installed — for testing whether a profile can
 *  launch on this host. */
export function availableAgentKinds(agents: AgentMetadata[]): Set<string> {
  return new Set(availableAgents(agents).map((agent) => agent.kind));
}

/** Whether the profile's agent harness is installed on this host. A profile
 *  naming an agent the server no longer lists counts as unavailable. */
export function profileAgentAvailable(profile: Profile, kinds: Set<string>): boolean {
  return kinds.has(profile.agent_kind);
}

/** `agents`, filtered to the installed harnesses, but with `current` kept in
 *  the list (at the end, if it was filtered out) so a selector bound to it
 *  never silently displays a different value than the one in effect. A synthetic
 *  entry is returned when the server does not describe `current` at all. */
export function agentOptionsWithCurrent(
  agents: AgentMetadata[],
  current: string | null | undefined,
): AgentMetadata[] {
  const options = availableAgents(agents);
  if (!current || options.some((agent) => agent.kind === current)) return options;
  const known = agents.find((agent) => agent.kind === current);
  return [
    ...options,
    known ?? ({ kind: current, label: current, available: false } as AgentMetadata),
  ];
}
