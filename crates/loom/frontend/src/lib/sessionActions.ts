import { ref } from 'vue';
import { post, patch, del } from '../api';
import type { LifecycleVerb } from './sessionState';

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

  const rename = (title: string) =>
    act('title', async () => {
      await patch(`/sessions/${getId()}`, { title });
      notice.value = 'Title saved.';
      await reload();
    });

  // Clear one tag — a chip's × removes that annotation entirely. The loud
  // `attention`/`triage` chips and the quiet free-form pills all clear through
  // here; clearing the agent's own `attention` is how a human marks a session
  // calm (calm is the tag's absence — there is no stored `ok`).
  const clearTag = (key: string) =>
    act(`tag:${key}`, async () => {
      await del(`/sessions/${getId()}/tags/${encodeURIComponent(key)}`);
      await reload();
    });

  const adopt = () =>
    act('adopt', async () => {
      await post(`/sessions/${getId()}/adopt`);
      notice.value = 'Session adopted — terminal session recreated.';
      await reload();
    });

  const archive = () =>
    act('archive', async () => {
      if (
        !confirm(
          'Archive this session? This tears down its terminal and removes the worktree, ' +
            'but keeps the branch and its weaver history for reference.',
        )
      )
        return;
      const res = (await post(`/sessions/${getId()}/archive`)) as { branch: string };
      notice.value = `Archived ${res.branch}.`;
      await reload();
    });

  const recover = () =>
    act('recover', async () => {
      await post(`/sessions/${getId()}/recover`);
      notice.value = 'Session recovered — worktree rebuilt and agent resumed.';
      await reload();
    });

  const remove = () =>
    act('remove', async () => {
      if (!confirm('Remove this session, its worktree and terminal session?')) return;
      await del(`/sessions/${getId()}`);
      if (removed) removed();
      else await reload();
    });

  // The four lifecycle verbs are only ever reached by name — a caller renders a
  // list of `LifecycleAction`s and invokes whichever one was clicked — so `run`
  // is the whole surface and the individual verbs stay internal.
  const run = (verb: LifecycleVerb) => ({ adopt, recover, archive, remove })[verb]();

  return { busy, notice, error, rename, clearTag, run };
}
