import { shallowRef } from 'vue';

// The phone navigation lives in AppRail (the app shell), while the warm session
// panes and their guarded route changes live in SessionDetail. Publish only the
// active page's small UI controller here: no session data is duplicated, and a
// cached/deactivated detail unregisters before another one takes over.
export type MobileSessionSurface = 'terminal' | 'conversation' | 'artifacts' | 'changes' | 'shells';

export interface MobileSessionNavigation {
  id: string;
  protocol: 'terminal' | 'acp';
  active: MobileSessionSurface;
  select: (surface: MobileSessionSurface) => void | Promise<void>;
  openDetails: () => void;
}

const mobileSessionNavigation = shallowRef<MobileSessionNavigation | null>(null);

export function useMobileSessionNavigation() {
  return mobileSessionNavigation;
}

export function publishMobileSessionNavigation(navigation: MobileSessionNavigation) {
  mobileSessionNavigation.value = navigation;
}

export function clearMobileSessionNavigation(id: string) {
  if (mobileSessionNavigation.value?.id === id) mobileSessionNavigation.value = null;
}
