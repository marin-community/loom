<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useId } from 'vue';

interface Choice {
  id: string;
  label: string;
}

const props = withDefaults(
  defineProps<{
    choices: Choice[];
    modelValue: string;
    disabled?: boolean;
    fieldClass?: string;
    testid?: string;
    id?: string;
    placeholder?: string;
    /** Selectable blank value, shown first. Absent when the field has no default. */
    emptyLabel?: string;
    /** Commit text that matches no choice. Model fields do; the others revert. */
    allowCustom?: boolean;
    /** Which way the option list prefers to grow. It flips when that side
     *  would leave the viewport, so a field on the screen edge opens inward. */
    placement?: 'left' | 'right';
  }>(),
  {
    disabled: false,
    fieldClass: 'bg-surface',
    testid: '',
    placeholder: 'Agent default',
    emptyLabel: '',
    allowCustom: false,
    placement: 'right',
  },
);

const emit = defineEmits<{
  'update:modelValue': [string];
}>();

const uid = useId();
const open = ref(false);
const editing = ref(false);
const query = ref('');
const activeOption = ref(-1);
const anchor = ref<HTMLInputElement | null>(null);
const menuStyle = ref<Record<string, string>>({});

/** The label of the current value, or its raw text when it is a custom model. */
const currentDisplay = computed(() => {
  const value = props.modelValue;
  if (!value) return '';
  return props.choices.find((choice) => choice.id === value)?.label ?? value;
});

const listed = computed<Choice[]>(() => {
  const choices = props.emptyLabel
    ? [{ id: '', label: props.emptyLabel }, ...props.choices]
    : props.choices;
  // Until the text differs from the current value, show the whole list so
  // opening the field works like a dropdown. Typing then narrows it.
  const q = query.value.trim().toLowerCase();
  if (!editing.value || q === currentDisplay.value.trim().toLowerCase()) return choices;
  if (!q) return choices;
  return choices.filter(
    (choice) => choice.id.toLowerCase().includes(q) || choice.label.toLowerCase().includes(q),
  );
});

function placeMenu() {
  const input = anchor.value;
  if (!input) return;
  const rect = input.getBoundingClientRect();
  const availableRight = window.innerWidth - rect.left - 8;
  const availableLeft = rect.right - 8;
  const preferred = props.placement === 'left' ? availableLeft : availableRight;
  const other = props.placement === 'left' ? availableRight : availableLeft;
  const growLeft =
    props.placement === 'left'
      ? preferred >= other || preferred >= rect.width * 2
      : other > preferred && preferred < rect.width * 2;
  const available = growLeft ? availableLeft : availableRight;
  const spaceBelow = window.innerHeight - rect.bottom;
  const openUp = spaceBelow < 224 && rect.top > spaceBelow;
  menuStyle.value = {
    top: openUp ? 'auto' : `${rect.bottom + 4}px`,
    bottom: openUp ? `${window.innerHeight - rect.top + 4}px` : 'auto',
    left: growLeft ? 'auto' : `${rect.left}px`,
    right: growLeft ? `${window.innerWidth - rect.right}px` : 'auto',
    minWidth: `${rect.width}px`,
    maxWidth: `${Math.max(rect.width, Math.min(rect.width * 2, available))}px`,
  };
}

function startTracking() {
  window.addEventListener('scroll', placeMenu, true);
  window.addEventListener('resize', placeMenu);
}

function stopTracking() {
  window.removeEventListener('scroll', placeMenu, true);
  window.removeEventListener('resize', placeMenu);
}

const inputValue = computed(() => (editing.value ? query.value : currentDisplay.value));

onBeforeUnmount(stopTracking);

function optionId(index: number): string {
  return `${uid}-option-${index}`;
}

function onFocus() {
  if (props.disabled) return;
  editing.value = true;
  query.value = currentDisplay.value;
  open.value = true;
  activeOption.value = -1;
  startTracking();
  void nextTick(placeMenu);
}

function onInput(event: Event) {
  editing.value = true;
  query.value = (event.target as HTMLInputElement).value;
  open.value = true;
  activeOption.value = -1;
}

function onBlur() {
  commit();
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    if (!open.value) return;
    event.stopPropagation();
    editing.value = false;
    open.value = false;
    activeOption.value = -1;
    stopTracking();
    return;
  }
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    if (!open.value) {
      event.preventDefault();
      event.stopPropagation();
      editing.value = true;
      query.value = currentDisplay.value;
      open.value = true;
      startTracking();
      void nextTick(placeMenu);
      return;
    }
    const count = listed.value.length;
    if (!count) return;
    event.preventDefault();
    event.stopPropagation();
    const delta = event.key === 'ArrowDown' ? 1 : -1;
    activeOption.value =
      activeOption.value < 0
        ? delta > 0
          ? 0
          : count - 1
        : (activeOption.value + delta + count) % count;
    void nextTick(() => {
      document.getElementById(optionId(activeOption.value))?.scrollIntoView({ block: 'nearest' });
    });
    return;
  }
  if (event.key !== 'Enter') return;
  event.preventDefault();
  event.stopPropagation();
  if (activeOption.value >= 0) pick(listed.value[activeOption.value]);
  else commit();
}

function closeMenu() {
  editing.value = false;
  open.value = false;
  activeOption.value = -1;
  stopTracking();
}

function pick(choice: Choice) {
  emit('update:modelValue', choice.id);
  closeMenu();
}

/** The value to commit from the current edit. `null` reverts the field. */
function commitValue(): string | null {
  const q = query.value.trim();
  if (!q) return props.emptyLabel ? '' : null;
  if (props.emptyLabel && props.emptyLabel.toLowerCase() === q.toLowerCase()) return '';
  const exact = props.choices.find(
    (choice) =>
      choice.id.toLowerCase() === q.toLowerCase() || choice.label.toLowerCase() === q.toLowerCase(),
  );
  if (exact) return exact.id;
  return props.allowCustom ? q : null;
}

/** Commit the pending edit, skipping the emit when nothing changed. */
function commit() {
  if (!editing.value) return;
  const next = commitValue();
  if (next !== null && next !== props.modelValue) emit('update:modelValue', next);
  closeMenu();
}
</script>

<template>
  <div>
    <input
      :id="id"
      ref="anchor"
      role="combobox"
      aria-autocomplete="list"
      :aria-expanded="open && listed.length > 0"
      :aria-controls="`${uid}-options`"
      :aria-activedescendant="activeOption >= 0 ? optionId(activeOption) : undefined"
      :value="inputValue"
      :disabled="disabled"
      :data-testid="testid || undefined"
      autocomplete="off"
      spellcheck="false"
      :placeholder="placeholder"
      :title="modelValue || undefined"
      class="min-w-0 w-full rounded px-2 py-1.5 text-fg outline-none focus:ring-1 ring-accent disabled:opacity-60"
      :class="fieldClass"
      @focus="onFocus"
      @input="onInput"
      @blur="onBlur"
      @keydown="onKeydown"
    />
    <Teleport to="body">
      <ul
        v-if="open && listed.length"
        :id="`${uid}-options`"
        role="listbox"
        data-testid="model-options"
        data-choice-menu
        class="fixed z-50 max-h-56 w-max overflow-auto rounded border border-line bg-input shadow-lg"
        :style="menuStyle"
      >
        <li v-for="(choice, index) in listed" :key="choice.id || `${uid}-empty`">
          <button
            :id="optionId(index)"
            type="button"
            role="option"
            :aria-selected="activeOption === index"
            data-testid="model-option"
            :title="choice.id"
            @mousedown.prevent="pick(choice)"
            class="block w-full min-w-0 whitespace-nowrap px-2 py-1.5 text-left text-sm text-fg hover:bg-subtle"
            :class="{ 'bg-subtle': activeOption === index }"
          >
            {{ choice.label }}
          </button>
        </li>
      </ul>
    </Teleport>
  </div>
</template>
