<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { get, patch, listAgents } from '../api';
import type { AgentMetadata, CustomAgent, SettingView } from '../types';
import ToggleSwitch from '../components/ToggleSwitch.vue';
import TokensPanel from '../components/TokensPanel.vue';
import AccountPanel from '../components/AccountPanel.vue';
import EnvPanel from '../components/EnvPanel.vue';
import LogsPanel from '../components/LogsPanel.vue';
import AgentProfileEditor from '../components/AgentProfileEditor.vue';
import CustomAgentsPanel from '../components/CustomAgentsPanel.vue';
import AppearancePanel from '../components/AppearancePanel.vue';
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
  | 'watches'
  | 'workspace'
  | 'environment'
  | 'access'
  | 'diagnostics';

interface CategoryItem {
  id: Category;
  label: string;
  groups?: string[];
  summary: string;
}

const categories: CategoryItem[] = [
  {
    id: 'agents',
    label: 'Agents',
    groups: ['Agents'],
    summary: 'Default runtime profiles and custom agents for new work sessions.',
  },
  {
    id: 'sessions',
    label: 'Sessions',
    groups: ['Server', 'Sessions'],
    summary: 'Server recovery, launch-time behavior, setup budgets, and conversation logs.',
  },
  {
    id: 'github',
    label: 'GitHub',
    groups: ['GitHub'],
    summary: 'Pull request polling, merge archiving, and issue-comment launch triggers.',
  },
  {
    id: 'watches',
    label: 'Watches',
    groups: ['Watch'],
    summary: 'Fleet watcher defaults and engine-level safety controls.',
  },
  {
    id: 'workspace',
    label: 'Workspace',
    groups: ['Editor'],
    summary: 'Embedded editor behavior plus terminal appearance and typography.',
  },
  {
    id: 'environment',
    label: 'Environment',
    summary: 'Environment variables exported into future agent sessions.',
  },
  {
    id: 'access',
    label: 'Access',
    groups: ['Authentication'],
    summary: 'Identity, approved users, browser authentication, GitHub App, and API tokens.',
  },
  {
    id: 'diagnostics',
    label: 'Diagnostics',
    summary:
      'Background tasks, live server logs, and build status for debugging this loom deployment.',
  },
];

const agentKeys = { agent: 'agent.default', model: 'agent.model', effort: 'agent.effort' };
const agentProfileTitle = 'Session default runtime';

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
  const groups = currentCategory.value.groups ?? [];
  return groups
    .flatMap((group) => groupedSettings.value.get(group) ?? [])
    .sort((a, b) => a.label.localeCompare(b.label));
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

function availableAgents(): AgentMetadata[] {
  return agents.value;
}

function selectedAgent(): AgentMetadata | undefined {
  const kind = drafts.value[agentKeys.agent];
  return availableAgents().find((agent) => agent.kind === kind);
}

function sanitizeAgentDraft() {
  const choices = availableAgents();
  if (!choices.length) return;
  if (!choices.some((agent) => agent.kind === drafts.value[agentKeys.agent])) {
    drafts.value[agentKeys.agent] = choices[0].kind;
  }
  const agent = selectedAgent();
  if (!agent) return;
  if (
    drafts.value[agentKeys.model] &&
    !agent.accepts_raw_model &&
    !agent.models.some((choice) => choice.id === drafts.value[agentKeys.model])
  ) {
    drafts.value[agentKeys.model] = '';
  }
  if (
    drafts.value[agentKeys.effort] &&
    !agent.efforts.some((choice) => choice.id === drafts.value[agentKeys.effort])
  ) {
    drafts.value[agentKeys.effort] = '';
  }
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
    sanitizeAgentDraft();
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
    sanitizeAgentDraft();
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
  sanitizeAgentDraft();
}

function patchBody(keys: string[], reset = false): Record<string, string | null> {
  return Object.fromEntries(keys.map((key) => [key, reset ? null : (drafts.value[key] ?? '')]));
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

function profileKeys(): string[] {
  return Object.values(agentKeys);
}

function setAgent(kind: string) {
  drafts.value[agentKeys.agent] = kind;
  sanitizeAgentDraft();
}

function setProfileChoice(key: 'model' | 'effort', value: string) {
  drafts.value[agentKeys[key]] = value;
  sanitizeAgentDraft();
}

function profileIsDefault(): boolean {
  return profileKeys().every((key) => setting(key)?.is_default);
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

watch(() => [drafts.value['agent.default'], agents.value.length], sanitizeAgentDraft);

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
              item.id === 'environment'
                ? 'settings-tab-env'
                : item.id === 'access'
                  ? 'settings-tab-access'
                  : `settings-category-${item.id}`
            "
            class="flex w-full items-center gap-2 border-b border-line border-l-2 px-3 py-2 text-left text-sm last:border-b-0"
            :class="
              category === item.id
                ? 'border-l-accent bg-input font-medium text-fg'
                : 'border-l-transparent text-muted hover:bg-subtle hover:text-fg'
            "
            @click="setCategory(item.id)"
          >
            {{ item.label }}
          </button>
        </div>
      </aside>

      <main class="min-w-0">
        <header class="mb-3 border-b border-line pb-2">
          <div class="flex items-center gap-2">
            <h2 class="text-base font-semibold tracking-tight">{{ currentCategory.label }}</h2>
            <span
              v-if="currentCategory.groups?.length"
              class="rounded bg-input px-1.5 py-0.5 font-mono text-2xs text-faint"
            >
              {{ currentCategory.groups?.join(' + ') }}
            </span>
          </div>
          <p class="mt-0.5 text-xs text-muted">{{ currentCategory.summary }}</p>
        </header>

        <EnvPanel v-if="category === 'environment'" />
        <LogsPanel v-else-if="category === 'diagnostics'" />

        <div v-else-if="category === 'agents'" class="space-y-4">
          <AgentProfileEditor
            :title="agentProfileTitle"
            note="Used when a new work session does not specify an agent."
            :keys="agentKeys"
            :agents="availableAgents()"
            :agent-kind="drafts[agentKeys.agent] ?? ''"
            :model="drafts[agentKeys.model] ?? ''"
            :effort="drafts[agentKeys.effort] ?? ''"
            :dirty="dirtyKeys(profileKeys()).length > 0"
            :is-default="profileIsDefault()"
            :busy="busy === agentProfileTitle"
            @update-agent="setAgent"
            @update-model="(value) => setProfileChoice('model', value)"
            @update-effort="(value) => setProfileChoice('effort', value)"
            @save="saveKeys(profileKeys(), agentProfileTitle)"
            @reset="resetKeys(profileKeys(), agentProfileTitle)"
          />
          <CustomAgentsPanel :agents="customAgents" @reload="reloadAgents" />
        </div>

        <div v-else class="space-y-3">
          <AccountPanel v-if="category === 'access'" />
          <TokensPanel v-if="category === 'access'" />
          <AppearancePanel v-if="category === 'workspace'" />

          <section
            v-if="currentSettings.length"
            class="overflow-hidden rounded-md border border-line bg-surface"
          >
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

          <p
            v-if="
              !currentSettings.length && !error && category !== 'access' && category !== 'workspace'
            "
            class="text-sm text-muted"
          >
            Loading…
          </p>
        </div>
      </main>
    </div>
  </div>
</template>
