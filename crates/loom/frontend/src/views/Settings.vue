<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { invokeOperation, listAgents } from '../api';
import type { CustomAgent, SettingsEnvelope, SettingView } from '../types';
import ToggleSwitch from '../components/ToggleSwitch.vue';
import TokensPanel from '../components/TokensPanel.vue';
import AccountPanel from '../components/AccountPanel.vue';
import GithubConnectionPanel from '../components/GithubConnectionPanel.vue';
import SlackPanel from '../components/SlackPanel.vue';
import EnvPanel from '../components/EnvPanel.vue';
import LogsPanel from '../components/LogsPanel.vue';
import ProfilesPanel from '../components/ProfilesPanel.vue';
import McpPanel from '../components/McpPanel.vue';
import CustomAgentsPanel from '../components/CustomAgentsPanel.vue';
import AppearancePanel from '../components/AppearancePanel.vue';
import SettingFieldRow from '../components/SettingFieldRow.vue';
import UsersPanel from '../components/UsersPanel.vue';
import { me } from '../auth';

const route = useRoute();
const router = useRouter();

type Category =
  | 'account'
  | 'preferences'
  | 'diagnostics'
  | 'people'
  | 'agents'
  | 'integrations'
  | 'runtime'
  | 'automation';

type CategoryScope = 'personal' | 'user' | 'admin';

interface CategoryItem {
  id: Category;
  label: string;
  groups?: string[];
  summary: string;
  scope: CategoryScope;
}

const categories: CategoryItem[] = [
  {
    id: 'account',
    label: 'Account',
    summary: 'Your identity, password, personal GitHub credential, and API tokens.',
    scope: 'personal',
  },
  {
    id: 'preferences',
    label: 'Preferences',
    summary: 'Your terminal appearance, with deployment defaults available at any time.',
    scope: 'personal',
  },
  {
    id: 'diagnostics',
    label: 'Diagnostics',
    summary: 'Background tasks, server logs, and build status for debugging this deployment.',
    scope: 'user',
  },
  {
    id: 'people',
    label: 'People & security',
    groups: ['Authentication'],
    summary: 'Approved users, roles, sign-in policy, and deployment authentication.',
    scope: 'admin',
  },
  {
    id: 'agents',
    label: 'Agents & profiles',
    groups: ['Agents', 'Metadata'],
    summary:
      'Launch profiles, shared session environment, MCP capabilities, custom agents, and metadata assistance.',
    scope: 'admin',
  },
  {
    id: 'integrations',
    label: 'Integrations',
    groups: ['GitHub', 'Slack'],
    summary: 'Deployment-owned GitHub and Slack connections and trigger behavior.',
    scope: 'admin',
  },
  {
    id: 'runtime',
    label: 'Runtime',
    groups: ['Server', 'Sessions', 'Editor', 'Appearance'],
    summary:
      'Server recovery, session launch behavior, setup budgets, editor defaults, and inherited terminal defaults.',
    scope: 'admin',
  },
  {
    id: 'automation',
    label: 'Automation',
    groups: ['Watch', 'Automation'],
    summary: 'Watcher defaults, automation credentials, and engine-level safety controls.',
    scope: 'admin',
  },
];

function categoryFromQuery(q: unknown): Category {
  const match = categories.find((item) => item.id === q);
  return match && (match.scope !== 'admin' || me.role === 'admin') ? match.id : 'account';
}

const category = ref<Category>(categoryFromQuery(route.query.tab));
const settings = ref<SettingView[]>([]);
const customAgents = ref<CustomAgent[]>([]);
const profilesKey = ref(0);
const drafts = ref<Record<string, string>>({});
const error = ref('');
const notice = ref('');
const busy = ref('');

const currentCategory = computed(
  () => categories.find((item) => item.id === category.value) ?? categories[0],
);
const personalCategories = computed(() => categories.filter((item) => item.scope === 'personal'));
const userCategories = computed(() => categories.filter((item) => item.scope === 'user'));
const adminCategories = computed(() =>
  me.role === 'admin' ? categories.filter((item) => item.scope === 'admin') : [],
);

watch(
  () => route.query.tab,
  (q) => (category.value = categoryFromQuery(q)),
);

function syncCategoryAccess() {
  const next = categoryFromQuery(route.query.tab);
  category.value = next;
  if (next === 'account' && route.query.tab) {
    router.replace({ query: { ...route.query, tab: undefined } });
  }
}

watch(() => me.role, syncCategoryAccess);

function setCategory(next: Category) {
  category.value = next;
  router.replace({
    query: { ...route.query, tab: next === 'account' ? undefined : next },
  });
}

function scopeLabel(scope: CategoryScope): string {
  if (scope === 'admin') return 'Administration';
  if (scope === 'user') return 'All users';
  return 'Personal';
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
  return s.source === 'default' && !dirty(s);
}

function dirty(s: SettingView): boolean {
  return drafts.value[s.key] !== s.value;
}

function sourceLabel(s: SettingView): string {
  return dirty(s) ? 'unsaved' : s.source;
}

function inheritedValue(s: SettingView): string {
  return s.deployment_value ?? s.default;
}

function canReset(s: SettingView): boolean {
  return dirty(s) || s.source === 'runtime';
}

function dirtyKeys(keys: string[]): string[] {
  return keys.filter((key) => drafts.value[key] !== setting(key)?.value);
}

function defaultText(value: string): string {
  return value || '(empty)';
}

async function load() {
  try {
    const [res, agentRes] = await Promise.all([invokeOperation('settings.get', {}), listAgents()]);
    if (!Array.isArray(res?.settings)) {
      throw new Error('Unexpected settings.get response — the server may be out of date.');
    }
    settings.value = res.settings;
    customAgents.value = agentRes.custom;
    drafts.value = Object.fromEntries(res.settings.map((s) => [s.key, s.value]));
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
    customAgents.value = res.custom;
    profilesKey.value += 1;
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
}

function patchBody(keys: string[], reset = false): Record<string, string | null> {
  return Object.fromEntries(keys.map((key) => [key, reset ? null : (drafts.value[key] ?? '')]));
}

async function saveKeys(keys: string[], label: string) {
  const changed = dirtyKeys(keys);
  if (!changed.length) return;
  await act(label, async () => {
    const res = await invokeOperation('settings.patch', { changes: patchBody(changed) });
    adopt(res, changed);
    notice.value = `Saved ${label}.`;
  });
}

async function resetKeys(keys: string[], label: string) {
  await act(label, async () => {
    const res = await invokeOperation('settings.patch', { changes: patchBody(keys, true) });
    adopt(res, keys);
    notice.value = `Reset ${label}.`;
  });
}

const saveSetting = (s: SettingView) => saveKeys([s.key], s.label);
const resetSetting = (s: SettingView) => resetKeys([s.key], s.label);

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

onMounted(() => {
  syncCategoryAccess();
  load();
});
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
        <template
          v-for="section in [
            { label: 'Personal', items: personalCategories },
            { label: 'Operations', items: userCategories },
            { label: 'Administration', items: adminCategories },
          ]"
          :key="section.label"
        >
          <div v-if="section.items.length" class="mb-4">
            <p class="mb-1 px-1 text-2xs font-semibold uppercase tracking-wider text-faint">
              {{ section.label }}
            </p>
            <div class="overflow-hidden rounded-md border border-line bg-surface">
              <button
                v-for="item in section.items"
                :key="item.id"
                type="button"
                :data-testid="`settings-category-${item.id}`"
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
          </div>
        </template>
      </aside>

      <main class="min-w-0">
        <header class="mb-3 border-b border-line pb-2">
          <div class="flex items-center gap-2">
            <h2 class="text-base font-semibold tracking-tight">{{ currentCategory.label }}</h2>
            <span class="rounded bg-input px-1.5 py-0.5 text-2xs text-faint">
              {{ scopeLabel(currentCategory.scope) }}
            </span>
          </div>
          <p class="mt-0.5 text-xs text-muted">{{ currentCategory.summary }}</p>
        </header>

        <LogsPanel v-if="category === 'diagnostics'" />

        <div v-else class="space-y-3">
          <template v-if="category === 'agents'">
            <ProfilesPanel :key="profilesKey" />
            <EnvPanel />
            <McpPanel />
            <CustomAgentsPanel :agents="customAgents" @reload="reloadAgents" />
          </template>
          <template v-if="category === 'account'">
            <AccountPanel />
            <TokensPanel />
          </template>
          <AppearancePanel v-if="category === 'preferences'" />
          <UsersPanel v-if="category === 'people'" />
          <GithubConnectionPanel v-if="category === 'integrations'" />
          <SlackPanel v-if="category === 'integrations'" />

          <section
            v-if="currentSettings.length"
            :data-testid="category === 'agents' ? 'metadata-settings' : undefined"
            class="overflow-hidden rounded-md border border-line bg-surface"
          >
            <SettingFieldRow
              v-for="s in currentSettings"
              :key="s.key"
              :setting="s"
              :inherited-label="defaultText(inheritedValue(s))"
              :source-label="sourceLabel(s)"
              :is-default="isDefaultValue(s)"
              :can-reset="canReset(s)"
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
                <textarea
                  v-if="s.kind === 'text'"
                  :id="s.key"
                  v-model="drafts[s.key]"
                  rows="5"
                  :placeholder="defaultText(inheritedValue(s))"
                  class="w-full resize-y rounded bg-input px-2 py-1 font-mono text-sm outline-none ring-accent focus:ring-1"
                />
                <input
                  v-else
                  :id="s.key"
                  v-model="drafts[s.key]"
                  :type="s.kind === 'int' ? 'number' : 'text'"
                  :placeholder="defaultText(inheritedValue(s))"
                  class="w-full rounded bg-input px-2 py-1 text-sm outline-none ring-accent focus:ring-1"
                  :class="{ 'font-mono': s.kind === 'string' }"
                />
              </div>
            </SettingFieldRow>
          </section>

          <p
            v-if="currentCategory.groups?.length && !currentSettings.length && !error"
            class="text-sm text-muted"
          >
            Loading…
          </p>
        </div>
      </main>
    </div>
  </div>
</template>
