import { ref } from 'vue';
import {
  archiveSession,
  clearSessionTag,
  patch,
  post,
  put,
  regenerateSessionTitle,
  removeSession,
  setSessionTitleGeneration,
} from '../api';
import { confirmAction } from './confirmation';
import { AUTO_ARCHIVE_DISABLED_VALUE, AUTO_ARCHIVE_KEY, type LifecycleVerb } from './sessionState';

// The session's write surface, shared by every place that can act on a session:
// the detail page's header and the fleet list's per-row menu.
//
//   rename       — the one human-authored branch field (the workstream label)
//   clearTag     — delete any one tag, loud or quiet (a chip's × clears it);
//                  clearing the agent's `attention` is how a human marks it calm
//   adopt        — recreate the terminal for an orphaned session
//   archive      — tear down terminal + worktree, keep the branch/history
//   recover      — rebuild an archived session's worktree and resume its agent
//                  (the inverse of archive — reuses the kept branch/history)
//   remove       — delete the session entirely
//
// The four lifecycle verbs above are exposed as `run(verb)`, so a caller
// rendering a list of `lifecycleActions()` can invoke whichever one was clicked
// without re-switching on the verb.
//
// `reload` is called after any write that mutates server state the caller shows.
// `removed` fires after a successful remove — the detail page routes back to the
// list (its subject is gone), the fleet list just refreshes in place. `busy`
// names the in-flight action (for per-button spinners); `notice`/`error` carry
// the last result.
export function useSessionActions(
  getId: () => string,
  reload: () => void | Promise<void>,
  removed?: () => void,
) {
  const busy = ref('');
  const notice = ref('');
  const error = ref('');

  async function act(name: string, fn: () => Promise<void>) {
    busy.value = name;
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

  const rename = (title: string, expectedTitle: string, expectedProvenance: string) =>
    act('title', async () => {
      await patch(`/sessions/${getId()}`, {
        title,
        expected_title: expectedTitle,
        expected_title_provenance: expectedProvenance,
      });
      notice.value = 'Title saved.';
      await reload();
    });

  const regenerateTitle = () =>
    act('title-generate', async () => {
      const updated = await regenerateSessionTitle(getId());
      notice.value =
        {
          idle: 'Task-label refresh is idle.',
          running: 'Task-label refresh started.',
          generated: 'Task label refreshed.',
          protected: 'Task label is protected by its human or issue source.',
          unavailable: 'Metadata assistance is unavailable for this session.',
          disabled: 'Generated task labels are disabled.',
          stale: 'Task-label source changed; stale output was discarded.',
          failed: 'Task-label refresh failed.',
        }[updated.title_generation.status] ??
        `Task-label refresh: ${updated.title_generation.status}.`;
      await reload();
    });

  const setTitleGeneration = (enabled: boolean) =>
    act('title-generation', async () => {
      await setSessionTitleGeneration(getId(), enabled);
      notice.value = `Generated task labels ${enabled ? 'enabled' : 'disabled'}.`;
      await reload();
    });

  // Clear one tag — a chip's × removes that annotation entirely. The loud
  // `attention`/`triage` chips and the quiet free-form pills all clear through
  // here; clearing the agent's own `attention` is how a human marks a session
  // calm (calm is the tag's absence — there is no stored `ok`).
  const clearTag = (key: string) =>
    act(`tag:${key}`, async () => {
      await clearSessionTag(getId(), key);
      await reload();
    });

  const setAutoArchiveDisabled = (disabled: boolean) =>
    act('auto-archive', async () => {
      const path = `/sessions/${getId()}/tags/${AUTO_ARCHIVE_KEY}`;
      if (disabled) {
        await put(path, {
          value: AUTO_ARCHIVE_DISABLED_VALUE,
          note: 'automatic archive disabled by user',
          by: 'manual',
        });
        notice.value = 'Automatic archive disabled for this session.';
      } else {
        await clearSessionTag(getId(), AUTO_ARCHIVE_KEY);
        notice.value = 'Automatic archive enabled for this session.';
      }
      await reload();
    });

  const adopt = () =>
    act('adopt', async () => {
      await post(`/sessions/${getId()}/adopt`);
      notice.value = 'Session adopted — terminal session recreated.';
      await reload();
    });

  const archive = () =>
    confirmAction({
      title: 'Archive this session?',
      description:
        'Its terminal and worktree will be removed. The branch, conversation, placement, and Weaver history remain recoverable in History.',
      confirmLabel: 'Archive session',
      danger: true,
      action: async () => {
        error.value = '';
        notice.value = '';
        const res = (await archiveSession(getId())) as { branch: string };
        notice.value = `Archived ${res.branch}.`;
        await reload();
      },
    });

  const recover = () =>
    act('recover', async () => {
      await post(`/sessions/${getId()}/recover`);
      notice.value = 'Session recovered — worktree rebuilt and agent resumed.';
      await reload();
    });

  const remove = () =>
    confirmAction({
      title: 'Permanently remove this session?',
      description:
        'Its terminal, worktree, Git branch, conversation, and Weaver history will be deleted. Claimed issues return to the backlog.',
      confirmLabel: 'Remove session',
      danger: true,
      action: async () => {
        error.value = '';
        notice.value = '';
        await removeSession(getId());
        if (removed) removed();
        else await reload();
      },
    });

  // The four lifecycle verbs are only ever reached by name — a caller renders a
  // list of `LifecycleAction`s and invokes whichever one was clicked — so `run`
  // is the whole surface and the individual verbs stay internal.
  const run = (verb: LifecycleVerb) => ({ adopt, recover, archive, remove })[verb]();

  return {
    busy,
    notice,
    error,
    rename,
    regenerateTitle,
    setTitleGeneration,
    clearTag,
    setAutoArchiveDisabled,
    run,
  };
}
