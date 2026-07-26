import { ref, type Ref } from 'vue';
import { ApiError } from '../api';
import type { SessionLayout } from '../types';

type Operation = (current: SessionLayout) => Promise<SessionLayout>;

/** One serialized optimistic command lane for the shared layout. */
export function useLayoutCommands(layout: Ref<SessionLayout | null>, refresh: () => Promise<void>) {
  const busy = ref(false);
  const error = ref('');
  let tail = Promise.resolve();

  function run(operation: Operation): Promise<boolean> {
    const result = tail.then(async () => {
      error.value = '';
      if (!layout.value) return false;
      busy.value = true;
      try {
        layout.value = await operation(layout.value);
        await refresh();
        return true;
      } catch (cause) {
        if (cause instanceof ApiError && cause.status === 409) {
          const current = cause.body.layout;
          if (current && typeof current === 'object') layout.value = current as SessionLayout;
          error.value =
            'The workbench changed in another client. Review the refreshed layout and try again.';
        } else {
          error.value = (cause as Error).message;
        }
        await refresh();
        return false;
      } finally {
        busy.value = false;
      }
    });
    tail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  return { busy, error, run };
}
