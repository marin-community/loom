import { ref } from 'vue';
import { useRouter } from 'vue-router';
import { post, patch, del } from '../api';

// The session's write surface, shared by every view that hosts the page header
// (SessionDetail and FileBrowser). The header is otherwise identical on both
// surfaces, so its lifecycle behaviour lives here once rather than being
// duplicated per parent.
//
//   rename       — the one human-authored branch field (the workstream label)
//   acknowledge  — clear the agent's attention signal back to `ok`
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

  const acknowledge = () =>
    act('acknowledge', async () => {
      await patch(`/sessions/${getId()}`, { attention: 'ok' });
      notice.value = 'Marked OK.';
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

  return { busy, notice, error, rename, acknowledge, adopt, archive, remove };
}
