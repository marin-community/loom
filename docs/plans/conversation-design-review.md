Loom should let an operator see what changed, inspect the evidence, and understand
what needs attention without reading every command. The Atlas conversation shows
where that hierarchy breaks: long commands occupy the activity list, screenshots
appear as paths, and a release failure ends with a small “failed” label.

The review uses that conversation, the current Vue components and palette tokens,
and desktop and phone renders of a scripted conversation with the supplied Atlas
screenshots. The script exercises the real ACP journal and renderer; its prose
and command error are illustrative.

| Priority | Observation | Recommended change |
| --- | --- | --- |
| First | Viewed images disappear inside tool disclosures. Codex supplies local resource links that Loom discarded. | Capture the images when the tool completes and show filename-labelled thumbnails under the activity group. Keep them visible while the group is closed. This change implements that behavior. |
| Next | A shell command is also its heading. Expanding an investigation can reveal long heredocs before the useful result. | Give tools a short action label and a separate command detail: “Capture screenshots”, “Update index.html”, “Release local preview”. Preserve structured tool names and inputs from adapters so labels can be reliable. Keep the exact command available for inspection and copying. |
| Next | A failure badge identifies an event but does not explain its consequence. | Lead failed steps with the failed action and the useful error, followed by the command and full output. Keep a tool failure distinct from a session needing a human; an agent may already be recovering. |
| Next | The composer takes a large share of the phone viewport, and model, effort, permissions, attachments, and command hints compete with the message. | Start with two text lines and grow while typing. Keep Send and Attach close to the input; move infrequent runtime settings into a labelled menu that exposes the current selection. Preserve queued-message and interruption controls. |
| Next | Every short agent update adds another speaker rule and timestamp. “1 thinking · 6 steps — 2 execute…” describes protocol activity. | Retain clear turn boundaries but use less spacing for progress within the same turn. Prefer plain labels such as “6 actions · 2 images”; keep reasoning and tool-kind counts inside the activity detail. |
| Later | The Context rail lists turns and the plan, but does not help revisit an image or failed step. | Add links to evidence and failures within the selected turn. Keep the rail optional and the prose measure bounded; leave the conversation full width on phones. |

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
Activity headings now use `--muted`. Extend that treatment to essential metadata,
placeholder text, and controls that currently depend on faint text. Check the
actual surface behind each control rather than applying the table to every
background. Keep saturated green concentrated in actions, focus, and healthy
state; use text and an icon alongside amber and red signals.

Image interaction should also work without hovering. The implemented thumbnails
open by click, tap, or keyboard; the viewer fits the image to the available width
and offers **Actual size** for inspection. Its controls retain keyboard focus,
Escape closes it, and focus returns to the thumbnail. Phone controls have a
44-pixel minimum height.

Local capture covers PNG, JPEG, GIF, WebP, and AVIF files up to 10 MiB in the
session worktree or the system temporary directory. Direct ACP image results
also remain supported. Previously discarded image bytes cannot be reconstructed
from the journal, and terminal transcript images still have no image payload.
The next broader conversation change should address tool labels and failure
presentation together; a whole-app palette replacement has less value than
making the current content easier to scan.
