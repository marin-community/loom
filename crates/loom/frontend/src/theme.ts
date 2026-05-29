import { ref } from 'vue';

export type Theme = 'light' | 'dark';

const STORAGE_KEY = 'loom-theme';

function preferred(): Theme {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === 'light' || stored === 'dark') return stored;
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function apply(t: Theme) {
  document.documentElement.classList.toggle('dark', t === 'dark');
}

export const theme = ref<Theme>(preferred());

// Apply once at module load so there's no flash of the wrong palette.
apply(theme.value);

export function setTheme(t: Theme) {
  theme.value = t;
  localStorage.setItem(STORAGE_KEY, t);
  apply(t);
}

export function toggleTheme() {
  setTheme(theme.value === 'dark' ? 'light' : 'dark');
}
