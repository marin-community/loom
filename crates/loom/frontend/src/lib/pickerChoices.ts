import type { AgentChoice, AgentMetadata } from '../types';

/** One selectable value in a picker. */
export type Choice = Pick<AgentChoice, 'id' | 'label'>;

/** A model that scopes effort per model owns the list; one that does not falls
 *  back to the agent's global effort list, which is all there is until a
 *  catalogue fetch fills the per-model choices. */
export function effortChoices(
  agent: AgentMetadata | undefined,
  model: AgentChoice | undefined,
): Choice[] {
  if (model?.efforts?.length) return model.efforts;
  return agent?.efforts ?? [];
}

/** Fast is always per model: an agent advertises none globally. */
export function fastChoices(model: AgentChoice | undefined): Choice[] {
  return model?.fast ?? [];
}

/** The mode options every ACP harness accepts. */
export const MODE_CHOICES: Choice[] = [
  'auto',
  'default',
  'acceptEdits',
  'plan',
  'bypassPermissions',
].map((mode) => ({ id: mode, label: mode }));

/** A harness that cannot speak ACP offers only the terminal protocol. */
export function protocolChoices(agent: AgentMetadata | undefined): Choice[] {
  return [
    ...(agent?.supports_acp !== false ? [{ id: 'acp', label: 'ACP' }] : []),
    { id: 'terminal', label: 'Terminal' },
  ];
}

/** Clear the model, effort and fast selections because the agent they belonged
 *  to is no longer the one in effect. */
export function clearedForAgentChange(): { model: string; effort: string; fast: string } {
  return { model: '', effort: '', fast: '' };
}

/** The selections to clear after picking `modelId`.
 *
 *  Only clears a selector the new model states its own choices for: an unfetched
 *  model says nothing about what the adapter accepts, so a value it might accept
 *  is kept rather than silently dropped. */
export function prunedForModelChange(
  model: AgentChoice | undefined,
  current: { effort: string; fast: string },
): { effort?: string; fast?: string } {
  const cleared: { effort?: string; fast?: string } = {};
  const efforts = model?.efforts ?? [];
  const fast = model?.fast ?? [];
  if (efforts.length && !efforts.some((choice) => choice.id === current.effort)) {
    cleared.effort = '';
  }
  if (fast.length && !fast.some((choice) => choice.id === current.fast)) {
    cleared.fast = '';
  }
  return cleared;
}

/** The model entry `modelId` names on `agent`, if any. */
export function modelById(
  agent: AgentMetadata | undefined,
  modelId: string,
): AgentChoice | undefined {
  return agent?.models.find((model) => model.id === modelId);
}
