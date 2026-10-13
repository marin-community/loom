import { computed, ref } from 'vue';
import { getPreferences } from '../api';
import type { UserPreferencesEnvelope } from '../types';

// The saved workbench selections the session list rehydrates from, as URL
// query params. The shell's "go to the fleet" affordances (rail, wordmark,
// status bar) link at that target directly, so clicking them lands on the
// configured list instead of bouncing from the bare one through the
// rehydrate rewrite. The session list keeps the snapshot fresh: every select
// change adopts here, and each restore adopts its fetched snapshot.

/** Query param -> personal-preference key for the four workbench selects. */
export const FILTER_PREFERENCE_KEYS = {
  status: 'workbench.status_filter',
  attention: 'workbench.attention_filter',
  creator: 'workbench.creator_filter',
  sort: 'workbench.sort',
} as const;
export type FilterPreferenceKey = keyof typeof FILTER_PREFERENCE_KEYS;
export const FILTER_QUERY_KEYS: FilterPreferenceKey[] = ['status', 'attention', 'creator', 'sort'];

// A blank selection (and manual sort) is the no-selection default, so it
// contributes no query param.
function contributes(key: FilterPreferenceKey, value: string): boolean {
  return !!value && (key !== 'sort' || value !== 'manual');
}

/** The URL query params a saved-preference snapshot translates to. */
export function filterQueryFromSaved(byKey: Record<string, string>): Record<string, string> {
  const query: Record<string, string> = {};
  for (const key of FILTER_QUERY_KEYS) {
    const value = byKey[FILTER_PREFERENCE_KEYS[key]];
    if (contributes(key, value)) query[key] = value;
  }
  return query;
}

const savedQuery = ref<Record<string, string>>({});

/** Adopt one just-saved selection, keeping the shell links in step. */
export function adoptFleetLinkPreference(key: FilterPreferenceKey, value: string): void {
  const next = { ...savedQuery.value };
  if (contributes(key, value)) next[key] = value;
  else delete next[key];
  savedQuery.value = next;
}

/** Adopt a whole fetched snapshot; returns the query params it translates to. */
export function adoptFleetLinkPreferences(byKey: Record<string, string>): Record<string, string> {
  const query = filterQueryFromSaved(byKey);
  savedQuery.value = query;
  return query;
}

/** A preferences envelope reduced to its `key -> value` map. */
export function preferenceSnapshot(envelope: UserPreferencesEnvelope): Record<string, string> {
  return Object.fromEntries(
    envelope.preferences.map((preference) => [preference.key, preference.value]),
  );
}

let primed: Promise<void> | null = null;
let primeGeneration = 0;

// One preferences fetch per signed-in shell, so the links are correct before
// the session list is ever opened.
export function primeFleetLink(): void {
  if (primed) return;
  const generation = ++primeGeneration;
  primed = getPreferences()
    .then((envelope) => {
      // Sign-out raced the fetch: the snapshot belongs to the previous
      // operator, so drop it instead of adopting it.
      if (generation === primeGeneration) adoptFleetLinkPreferences(preferenceSnapshot(envelope));
    })
    .catch(() => {});
}

// Sign-out: drop the snapshot and invalidate any still-in-flight prime, so a
// different operator's links never inherit it.
export function clearFleetLink(): void {
  primeGeneration++;
  primed = null;
  savedQuery.value = {};
}

/** The fleet-list route with the operator's saved selections applied. */
export const fleetRoute = computed(() => ({ path: '/', query: { ...savedQuery.value } }));
