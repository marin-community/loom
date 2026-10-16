Loom should let an operator see what changed, inspect the evidence, and understand
what needs attention without reading every command. The Atlas conversation shows
where that hierarchy breaks: long commands occupy the activity list, screenshots
appear as paths, and a release failure ends with a small “failed” label.

The review uses that conversation, the current Vue components and palette tokens,
and desktop and phone renders of a scripted conversation with illustrative
source-table and curriculum screenshots. The script exercises the real ACP
journal and renderer; its prose, example data, and command error are illustrative.

| Area | Observation | Implemented change |
| --- | --- | --- |
| Images | Viewed images disappear inside tool disclosures. Codex supplies local resource links that Loom discarded. | Capture images when the tool completes and show filename-labelled thumbnails under the activity group. They remain visible while the group is closed. |
| Tool labels | A shell command is also its heading. Expanding an investigation can reveal long heredocs before the useful result. | Prefer an adapter's description; otherwise show a concise command label such as “cargo test”. Preserve structured tool names and inputs in the live journal and archive. The exact command has its own bounded preview and full detail. |
| Failures | A failure badge identifies an event but does not explain its consequence. | Show the failed action's useful error before its command and full output, with a local red rule. A tool failure does not set session attention; the agent may already be recovering. |
| Composer | The composer takes a large share of the phone viewport, and runtime settings compete with the message. | Start with two text lines and grow while typing. Keep Attach and Send beside the input. Group model, effort, permissions, and other agent controls under Settings. Keep full access and pending permission changes visible. Drafts and queue/interruption controls retain their behavior. |
| Progress | Every short agent update adds another speaker rule and timestamp. “1 thinking · 6 steps — 2 execute…” describes protocol activity. | Keep one Agent heading per turn and reduce spacing for progress, preserving its order around actions. Summarize groups as “6 actions · 2 images”; retain reasoning and tool-kind counts inside the disclosure. |
| Context | The rail lists turns and the plan, but does not help revisit an image or failed step. | Link to images and failed actions in the turn being read. Following a failure reveals its detail. On phones, the rail closes and focus moves to the target. The rail remains optional and prose retains its bounded measure. |

The existing distinction between readable prose and monospace commands is useful.
Keep it. Tool glyphs should supplement an action label; the operator should not
need to learn what “⌗” or “❏” means. Consistent spacing and alignment can carry
most of the hierarchy without more cards, badges, or colored backgrounds.

The palettes have a clear identity. The immediate color issue is the use of
`--faint` for information that people need to read. These are calculated contrast
ratios against each palette's `--canvas`, using the source color values:

| Palette | `--faint` | `--muted` |
| --- | --- | --- |
| Light | 3.04:1 | 5.83:1 |
| Warm dark | 3.29:1 | 6.39:1 |
| Terminal dark | 3.52:1 | 7.83:1 |

Normal text needs at least 4.5:1 under
[WCAG's text contrast criterion](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html).
Activity headings, timestamps, essential metadata, placeholder text, and
conversation controls now use `--muted`. The existing palette tokens remain in
place. Saturated green stays concentrated in actions, focus, and healthy state;
failure text and a badge accompany the local red rule. Other application
surfaces still need their own contrast review against their actual backgrounds.

Image interaction should also work without hovering. The implemented thumbnails
open by click, tap, or keyboard; the viewer fits the image to the available width
and offers **Actual size** for inspection. Its controls retain keyboard focus,
Escape closes it, and focus returns to the thumbnail. Phone controls have a
44-pixel minimum height.

Local capture covers PNG, JPEG, GIF, WebP, and AVIF files up to 10 MiB in the
session worktree or the system temporary directory. Direct ACP image results
also remain supported. Previously discarded image bytes cannot be reconstructed
from the journal, and terminal transcript images still have no image payload.

Labels and failure summaries stay tied to adapter data. Loom does not infer the
intent of an arbitrary shell script: it uses a supplied description, a concise
command prefix, or a generic command label. The failure summary selects an error
line when available and leaves the full output accessible. Older journal entries
without structured inputs retain the complete execute title as command detail.
