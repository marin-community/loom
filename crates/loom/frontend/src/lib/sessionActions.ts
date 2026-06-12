import { ref } from 'vue';
import { useRouter } from 'vue-router';
import { post, patch, del } from '../api';

// The session's write surface, shared by every view that hosts the page header
// (SessionDetail and FileBrowser). The header is otherwise identical on both
// surfaces, so its lifecycle behaviour lives here once rather than being
// duplicated per parent.
//
//   rename       — the one human-authored branch field (the workstream label)
//   clearTag     — delete any one tag, loud or quiet (a chip's × clears it);
//                  clearing the agent's `attention` is how a human marks it calm
//   adopt        — recreate the tmux for an orphaned session
//   archive      — tear down tmux + worktree, keep the branch/history
//   remove        — delete the session entirely, then route back to the list
//
// `reload` is called after any write that mutates server state the page shows,
// so the caller can re-fetch. `busy` names the in-flight action (for per-button
// spinners); `notice`/`error` carry the last result.
export function useSessionActions(getId: () => string, reload: () => void | Promise<void>) {
  const router = useRouter();
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
      notice.value = 'Session adopted — tmux session recreated.';
      await reload();
    });

  const archive = () =>
    act('archive', async () => {
      if (
        !confirm(
          'Archive this session? This tears down its tmux and removes the worktree, ' +
            'but keeps the branch and its weaver history for reference.',
        )
      )
        return;
      const res = (await post(`/sessions/${getId()}/archive`)) as { branch: string };
      notice.value = `Archived ${res.branch}.`;
      await reload();
    });

  const remove = () =>
    act('remove', async () => {
      if (!confirm('Remove this session, its worktree and tmux session?')) return;
      await del(`/sessions/${getId()}`);
      router.push('/');
    });

  return { busy, notice, error, rename, clearTag, adopt, archive, remove };
}
