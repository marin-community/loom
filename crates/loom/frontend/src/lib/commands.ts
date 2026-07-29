import {
  computed,
  inject,
  onActivated,
  onBeforeUnmount,
  onDeactivated,
  onMounted,
  ref,
  toValue,
  type ComputedRef,
  type InjectionKey,
  type MaybeRefOrGetter,
  type Ref,
} from 'vue';

export interface Command {
  id: string;
  label: string;
  keys: string[];
  run: () => void | Promise<void>;
  enabled?: () => boolean;
  hint?: boolean;
}

interface CommandScope {
  id: string;
  label: string;
  priority: number;
  commands: MaybeRefOrGetter<Command[]>;
}

interface ActiveCommandScope extends Omit<CommandScope, 'commands'> {
  commands: Command[];
}

export interface CommandRegistry {
  activeScopes: ComputedRef<ActiveCommandScope[]>;
  hints: ComputedRef<Command[]>;
  chord: Ref<string>;
  helpOpen: Ref<boolean>;
  activate: (scope: CommandScope) => void;
  deactivate: (id: string) => void;
  dispatch: (event: KeyboardEvent) => boolean;
  clearChord: () => void;
  toggleHelp: () => void;
}

export const commandRegistryKey: InjectionKey<CommandRegistry> = Symbol('loom-commands');

function eventKey(event: KeyboardEvent): string {
  if (event.key === ' ') return 'Space';
  // Some automation/browser keyboard layouts report the physical slash key
  // with Shift separately instead of resolving it to the printable `?`.
  if (event.key === '/' && event.shiftKey) return '?';
  return event.key;
}

function ownsKeyboard(event: KeyboardEvent): boolean {
  return event.composedPath().some((node) => {
    if (!(node instanceof HTMLElement)) return false;
    return (
      node.matches(
        'input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="dialog"], [role="menu"], [role="listbox"], [data-command-capture]',
      ) || node.classList.contains('xterm')
    );
  });
}

export function createCommandRegistry(): CommandRegistry {
  const scopes = new Map<string, CommandScope>();
  const revision = ref(0);
  const chord = ref('');
  const helpOpen = ref(false);
  let chordTimer: number | undefined;

  function available(command: Command): boolean {
    return command.enabled?.() !== false;
  }

  const activeScopes = computed<ActiveCommandScope[]>(() => {
    revision.value;
    return [...scopes.values()]
      .sort((left, right) => right.priority - left.priority)
      .map((scope) => ({
        ...scope,
        commands: toValue(scope.commands).filter(available),
      }))
      .filter((scope) => scope.commands.length > 0);
  });

  const activeCommands = computed(() => {
    const seen = new Set<string>();
    const commands: Command[] = [];
    for (const scope of activeScopes.value) {
      for (const command of scope.commands) {
        if (seen.has(command.id)) continue;
        seen.add(command.id);
        commands.push(command);
      }
    }
    return commands;
  });
  const hints = computed(() => activeCommands.value.filter((command) => command.hint).slice(0, 5));

  function activate(scope: CommandScope) {
    if (scopes.get(scope.id) === scope) return;
    scopes.set(scope.id, scope);
    revision.value += 1;
  }

  function deactivate(id: string) {
    if (scopes.delete(id)) revision.value += 1;
  }

  function clearChord() {
    window.clearTimeout(chordTimer);
    chord.value = '';
  }

  function setChord(next: string) {
    clearChord();
    chord.value = next;
    chordTimer = window.setTimeout(clearChord, 1200);
  }

  function dispatch(event: KeyboardEvent): boolean {
    if (event.defaultPrevented) return false;

    if (helpOpen.value) {
      if (event.key !== 'Escape') return false;
      event.preventDefault();
      helpOpen.value = false;
      clearChord();
      return true;
    }

    if (ownsKeyboard(event)) return false;
    if (event.ctrlKey || event.metaKey || event.altKey) return false;

    const key = eventKey(event);
    if (key === 'Escape' && chord.value) {
      event.preventDefault();
      clearChord();
      return true;
    }

    const commands = activeCommands.value;
    const sequence = chord.value ? `${chord.value} ${key}` : key;
    const exact = commands.find((command) => command.keys.includes(sequence));
    if (exact) {
      event.preventDefault();
      clearChord();
      void exact.run();
      return true;
    }

    const prefix = commands.some((command) =>
      command.keys.some((binding) => binding.startsWith(`${sequence} `)),
    );
    if (prefix) {
      event.preventDefault();
      setChord(sequence);
      return true;
    }

    clearChord();
    return false;
  }

  function toggleHelp() {
    helpOpen.value = !helpOpen.value;
    clearChord();
  }

  return {
    activeScopes,
    hints,
    chord,
    helpOpen,
    activate,
    deactivate,
    dispatch,
    clearChord,
    toggleHelp,
  };
}

export function useCommandRegistry(): CommandRegistry {
  const registry = inject(commandRegistryKey);
  if (!registry) throw new Error('command registry is not provided');
  return registry;
}

/** Register commands for a mounted view. Keep-alive views are active only while visible. */
export function useCommandScope(
  id: string,
  label: string,
  commands: MaybeRefOrGetter<Command[]>,
  priority = 0,
) {
  const registry = useCommandRegistry();
  const scope = { id, label, commands, priority };
  onMounted(() => registry.activate(scope));
  onActivated(() => registry.activate(scope));
  onDeactivated(() => registry.deactivate(id));
  onBeforeUnmount(() => registry.deactivate(id));
}

export function useCommandDispatcher() {
  const registry = useCommandRegistry();
  onMounted(() => window.addEventListener('keydown', registry.dispatch));
  onBeforeUnmount(() => {
    window.removeEventListener('keydown', registry.dispatch);
    registry.clearChord();
  });
}
