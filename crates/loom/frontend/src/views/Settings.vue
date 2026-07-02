<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { get, patch, listAgents } from '../api';
import type { AgentMetadata, CustomAgent, SettingView } from '../types';
import ToggleSwitch from '../components/ToggleSwitch.vue';
import TokensPanel from '../components/TokensPanel.vue';
import AccountPanel from '../components/AccountPanel.vue';
import EnvPanel from '../components/EnvPanel.vue';
import AgentProfileEditor from '../components/AgentProfileEditor.vue';
import CustomAgentsPanel from '../components/CustomAgentsPanel.vue';
import SettingFieldRow from '../components/SettingFieldRow.vue';

const route = useRoute();
const router = useRouter();

// The server's canonical reply for both GET and PATCH /api/settings.
interface SettingsEnvelope {
  settings: SettingView[];
}

type Category =
  | 'agents'
  | 'sessions'
  | 'github'
  | 'authentication'
  | 'overlookers'
  | 'editor'
  | 'appearance'
  | 'env'
  | 'tokens'
  | 'account';

interface CategoryItem {
  id: Category;
  label: string;
  group?: string;
  summary: string;
}

const categories: CategoryItem[] = [
  {
    id: 'agents',
    label: 'Agents',
    group: 'Agents',
    summary: 'Default runtime profiles for new work sessions and the fleet concierge.',
  },
  {
    id: 'sessions',
    label: 'Sessions',
    group: 'Sessions',
    summary: 'Launch-time behavior, setup budgets, and archived conversation logs.',
  },
  {
    id: 'github',
    label: 'GitHub',
    group: 'GitHub',
    summary: 'Pull request polling, merge archiving, and issue-comment launch triggers.',
  },
  {
    id: 'authentication',
    label: 'Authentication',
    group: 'Authentication',
    summary: 'Browser and API authentication behavior for this loom server.',
  },
  {
    id: 'overlookers',
    label: 'Overlookers',
    group: 'Overlooker',
    summary: 'Fleet watcher defaults and engine-level safety controls.',
  },
  {
    id: 'editor',
    label: 'Editor',
    group: 'Editor',
    summary: 'Embedded code-server availability and lifecycle.',
  },
  {
    id: 'appearance',
    label: 'Appearance',
    group: 'Appearance',
    summary: 'Visual preferences for the workbench and terminal.',
  },
  {
    id: 'env',
    label: 'Environment',
    summary: 'Environment variables exported into future agent sessions.',
  },
  {
    id: 'tokens',
    label: 'Tokens',
    summary: 'Bearer tokens for automation and remote CLIs.',
  },
  {
    id: 'account',
    label: 'Account',
    summary: 'Signed-in identity, approved users, and GitHub App configuration.',
  },
];

type AgentSettingGroup = 'session' | 'concierge';

const agentGroups: Record<AgentSettingGroup, { agent: string; model: string; effort: string }> = {
  session: { agent: 'agent.default', model: 'agent.model', effort: 'agent.effort' },
  concierge: {
    agent: 'concierge.runtime',
    model: 'concierge.model',
    effort: 'concierge.effort',
  },
};

const agentProfiles: {
  id: AgentSettingGroup;
  title: string;
  note: string;
}[] = [
  {
    id: 'session',
    title: 'Session default runtime',
    note: 'Used when a new work session does not specify an agent.',
  },
  {
    id: 'concierge',
    title: 'Fleet concierge runtime',
    note: 'Used by Chat when it starts or resets the fleet concierge.',
  },
];

function categoryFromQuery(q: unknown): Category {
  return categories.some((item) => item.id === q) ? (q as Category) : 'agents';
}

const category = ref<Category>(categoryFromQuery(route.query.tab));
const settings = ref<SettingView[]>([]);
const agents = ref<AgentMetadata[]>([]);
const customAgents = ref<CustomAgent[]>([]);
const drafts = ref<Record<string, string>>({});
const error = ref('');
const notice = ref('');
const busy = ref('');

const currentCategory = computed(
  () => categories.find((item) => item.id === category.value) ?? categories[0],
);

watch(
  () => route.query.tab,
  (q) => (category.value = categoryFromQuery(q)),
);

function setCategory(next: Category) {
  category.value = next;
  router.replace({
    query: { ...route.query, tab: next === 'agents' ? undefined : next },
  });
}

const groupedSettings = computed(() => {
  const out = new Map<string, SettingView[]>();
  for (const s of settings.value) {
    const list = out.get(s.group);
    if (list) list.push(s);
    else out.set(s.group, [s]);
  }
  return out;
});

const currentSettings = computed(() => {
  const group = currentCategory.value.group;
  if (!group || group === 'Agents') return [];
  return groupedSettings.value.get(group) ?? [];
});

function setting(key: string): SettingView | undefined {
  return settings.value.find((s) => s.key === key);
}

function isDefaultValue(s: SettingView): boolean {
  return s.is_default && !dirty(s);
}

function dirty(s: SettingView): boolean {
  return drafts.value[s.key] !== s.value;
}

function dirtyKeys(keys: string[]): string[] {
  return keys.filter((key) => drafts.value[key] !== setting(key)?.value);
}

function defaultText(value: string): string {
  return value || '(empty)';
}

function agentsForGroup(group: AgentSettingGroup): AgentMetadata[] {
  return group === 'concierge'
    ? agents.value.filter((agent) => agent.supports_concierge)
    : agents.value;
}

function selectedAgent(group: AgentSettingGroup): AgentMetadata | undefined {
  const keys = agentGroups[group];
  const kind = drafts.value[keys.agent];
  return agentsForGroup(group).find((agent) => agent.kind === kind);
}

function sanitizeAgentDraft(group: AgentSettingGroup) {
  const keys = agentGroups[group];
  const availableAgents = agentsForGroup(group);
  if (!availableAgents.length) return;
  if (!availableAgents.some((agent) => agent.kind === drafts.value[keys.agent])) {
    drafts.value[keys.agent] = availableAgents[0].kind;
  }
  const agent = selectedAgent(group);
  if (!agent) return;
  if (
    drafts.value[keys.model] &&
    !agent.accepts_raw_model &&
    !agent.models.some((choice) => choice.id === drafts.value[keys.model])
  ) {
    drafts.value[keys.model] = '';
  }
  if (
    drafts.value[keys.effort] &&
    !agent.efforts.some((choice) => choice.id === drafts.value[keys.effort])
  ) {
    drafts.value[keys.effort] = '';
  }
}

function sanitizeAgentDrafts() {
  sanitizeAgentDraft('session');
  sanitizeAgentDraft('concierge');
}

async function load() {
  try {
    const [res, agentRes] = await Promise.all([
      get('/settings') as Promise<SettingsEnvelope>,
      listAgents(),
    ]);
    if (!Array.isArray(res?.settings)) {
      throw new Error('Unexpected /api/settings response — the server may be out of date.');
    }
    settings.value = res.settings;
    agents.value = agentRes.agents;
    customAgents.value = agentRes.custom;
    drafts.value = Object.fromEntries(res.settings.map((s) => [s.key, s.value]));
    sanitizeAgentDrafts();
    error.value = '';
  } catch (e) {
    settings.value = [];
    error.value = (e as Error).message;
  }
}

// Refresh the agent lists after a custom agent is added/edited/removed, without
// disturbing the settings drafts. A new or deleted agent changes the picker
// (`agents`) as well as the custom list, so both are refetched.
async function reloadAgents() {
  try {
    const res = await listAgents();
    agents.value = res.agents;
    customAgents.value = res.custom;
    sanitizeAgentDrafts();
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function act(key: string, fn: () => Promise<void>) {
  busy.value = key;
  error.value = '';
  notice.value = '';
  try {
    await fn();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = '';
  }
}

function adopt(res: SettingsEnvelope, changedKeys: string[]) {
  settings.value = res.settings;
  for (const changedKey of changedKeys) {
    const changed = res.settings.find((s) => s.key === changedKey);
    if (changed) drafts.value[changedKey] = changed.value;
  }
  sanitizeAgentDrafts();
}

function patchBody(keys: string[], reset = false): Record<string, string | null> {
  return Object.fromEntries(keys.map((key) => [key, reset ? null : drafts.value[key] ?? '']));
}

async function saveKeys(keys: string[], label: string) {
  const changed = dirtyKeys(keys);
  if (!changed.length) return;
  await act(label, async () => {
    const res = (await patch('/settings', patchBody(changed))) as SettingsEnvelope;
    adopt(res, changed);
    notice.value = `Saved ${label}.`;
  });
}

async function resetKeys(keys: string[], label: string) {
  await act(label, async () => {
    const res = (await patch('/settings', patchBody(keys, true))) as SettingsEnvelope;
    adopt(res, keys);
    notice.value = `Reset ${label}.`;
  });
}

const saveSetting = (s: SettingView) => saveKeys([s.key], s.label);
const resetSetting = (s: SettingView) => resetKeys([s.key], s.label);

function profileKeys(group: AgentSettingGroup): string[] {
  return Object.values(agentGroups[group]);
}

function setAgent(group: AgentSettingGroup, kind: string) {
  drafts.value[agentGroups[group].agent] = kind;
  sanitizeAgentDraft(group);
}

function setProfileChoice(group: AgentSettingGroup, key: 'model' | 'effort', value: string) {
  drafts.value[agentGroups[group][key]] = value;
  sanitizeAgentDraft(group);
}

function profileIsDefault(group: AgentSettingGroup): boolean {
  return profileKeys(group).every((key) => setting(key)?.is_default);
}

function durationOptions(s: SettingView): { label: string; value: string }[] {
  if (!s.key.endsWith('_secs')) return [];
  if (s.key.includes('cooldown')) {
    return [
      { label: 'Off', value: '0' },
      { label: '1m', value: '60' },
      { label: '5m', value: '300' },
      { label: '10m', value: '600' },
    ];
  }
  return [
    { label: '5m', value: '300' },
    { label: '10m', value: '600' },
    { label: '30m', value: '1800' },
    { label: '1h', value: '3600' },
  ];
}

watch(
  () => [drafts.value['agent.default'], drafts.value['concierge.runtime'], agents.value.length],
  sanitizeAgentDrafts,
);

onMounted(load);
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col px-5 py-3">
    <div class="mb-3 flex min-h-7 items-center gap-2.5">
      <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Settings</h1>
      <p v-if="notice" class="text-xs text-accent">{{ notice }}</p>
      <p v-if="error" class="text-xs text-block">{{ error }}</p>
    </div>

    <div class="grid min-h-0 flex-1 gap-4 lg:grid-cols-[13rem_minmax(0,58rem)]">
      <aside class="min-w-0">
        <div class="overflow-hidden rounded-md border border-line bg-surface">
          <button
            v-for="item in categories"
            :key="item.id"
            type="button"
            :data-testid="
              item.id === 'env'
                ? 'settings-tab-env'
                : item.id === 'tokens'
                  ? 'settings-tab-tokens'
                  : item.id === 'account'
                    ? 'settings-tab-account'
                    : `settings-category-${item.id}`
            "
            class="flex w-full items-center gap-2 border-b border-line px-3 py-2 text-left text-sm last:border-0"
            :class="
              category === item.id
                ? 'bg-input font-medium text-fg'
                : 'text-muted hover:bg-subtle hover:text-fg'
            "
            @click="setCategory(item.id)"
          >
            <span
              class="h-1.5 w-1.5 rounded-full"
              :class="
                item.id === 'agents'
                  ? 'bg-agent-line'
                  : item.id === 'github'
                    ? 'bg-ok-line'
                    : item.id === 'authentication'
                      ? 'bg-attn-line'
                      : item.id === 'tokens'
                        ? 'bg-info-line'
                        : 'bg-line'
              "
              aria-hidden="true"
            ></span>
            {{ item.label }}
          </button>
        </div>
      </aside>

      <main class="min-w-0">
        <header class="mb-3 border-b border-line pb-2">
          <div class="flex items-center gap-2">
            <h2 class="text-base font-semibold tracking-tight">{{ currentCategory.label }}</h2>
            <span
              v-if="currentCategory.group"
              class="rounded bg-input px-1.5 py-0.5 font-mono text-2xs text-faint"
            >
              {{ currentCategory.group }}
            </span>
          </div>
          <p class="mt-0.5 text-xs text-muted">{{ currentCategory.summary }}</p>
        </header>

        <EnvPanel v-if="category === 'env'" />
        <TokensPanel v-else-if="category === 'tokens'" />
        <AccountPanel v-else-if="category === 'account'" />

        <div v-else-if="category === 'agents'" class="space-y-4">
          <AgentProfileEditor
            v-for="profile in agentProfiles"
            :key="profile.id"
            :title="profile.title"
            :note="profile.note"
            :keys="agentGroups[profile.id]"
            :agents="agentsForGroup(profile.id)"
            :agent-kind="drafts[agentGroups[profile.id].agent] ?? ''"
            :model="drafts[agentGroups[profile.id].model] ?? ''"
            :effort="drafts[agentGroups[profile.id].effort] ?? ''"
            :dirty="dirtyKeys(profileKeys(profile.id)).length > 0"
            :is-default="profileIsDefault(profile.id)"
            :busy="busy === profile.title"
            @update-agent="(kind) => setAgent(profile.id, kind)"
            @update-model="(value) => setProfileChoice(profile.id, 'model', value)"
            @update-effort="(value) => setProfileChoice(profile.id, 'effort', value)"
            @save="saveKeys(profileKeys(profile.id), profile.title)"
            @reset="resetKeys(profileKeys(profile.id), profile.title)"
          />
          <CustomAgentsPanel :agents="customAgents" @reload="reloadAgents" />
        </div>

        <div v-else class="space-y-3">
          <section class="overflow-hidden rounded-md border border-line bg-surface">
            <SettingFieldRow
              v-for="s in currentSettings"
              :key="s.key"
              :setting="s"
              :default-label="defaultText(s.default)"
              :is-default="isDefaultValue(s)"
              :dirty="dirty(s)"
              :busy="busy === s.label"
              @save="saveSetting(s)"
              @reset="resetSetting(s)"
            >
              <div v-if="s.kind === 'bool'" class="flex min-w-0 flex-1 items-center gap-2">
                <ToggleSwitch
                  :id="s.key"
                  :model-value="drafts[s.key] === 'true'"
                  @update:model-value="drafts[s.key] = $event ? 'true' : 'false'"
                />
                <span class="text-xs text-muted">
                  {{ drafts[s.key] === 'true' ? 'Enabled' : 'Disabled' }}
                </span>
              </div>

              <div v-else-if="s.kind === 'enum'" class="flex min-w-0 flex-1 flex-wrap gap-1.5">
                <button
                  v-for="opt in s.options"
                  :key="opt"
                  type="button"
                  class="rounded border px-2.5 py-1 text-xs capitalize"
                  :class="
                    drafts[s.key] === opt
                      ? 'border-accent bg-accent text-accent-fg'
                      : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
                  "
                  @click="drafts[s.key] = opt"
                >
                  {{ opt }}
                </button>
              </div>

              <div v-else class="min-w-0 flex-1">
                <div v-if="durationOptions(s).length" class="mb-1 flex flex-wrap gap-1">
                  <button
                    v-for="opt in durationOptions(s)"
                    :key="opt.value"
                    type="button"
                    class="rounded border px-2 py-0.5 text-2xs"
                    :class="
                      drafts[s.key] === opt.value
                        ? 'border-accent bg-accent text-accent-fg'
                        : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
                    "
                    @click="drafts[s.key] = opt.value"
                  >
                    {{ opt.label }}
                  </button>
                </div>
                <input
                  :id="s.key"
                  v-model="drafts[s.key]"
                  :type="s.kind === 'int' ? 'number' : 'text'"
                  :placeholder="defaultText(s.default)"
                  class="w-full rounded bg-input px-2 py-1 text-sm outline-none ring-accent focus:ring-1"
                  :class="{ 'font-mono': s.kind === 'string' }"
                />
              </div>
            </SettingFieldRow>
          </section>

          <p v-if="!currentSettings.length && !error" class="text-sm text-muted">Loading…</p>
        </div>
      </main>
    </div>
  </div>
</template>
