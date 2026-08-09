export interface DiffLine {
  sign: '-' | '+';
  text: string;
}

function textLines(text: string): string[] {
  if (!text) return [];
  const withoutFinalNewline = text.endsWith('\n') ? text.slice(0, -1) : text;
  return withoutFinalNewline.split('\n');
}

/**
 * Return only the changed lines between the before and after snapshots in an
 * ACP diff content block. Providers such as Codex send the complete file in
 * oldText/newText; treating those snapshots as ready-made diff hunks makes a
 * one-line edit look like a whole-file replacement.
 */
export function changedDiffLines(oldText: string | null, newText: string): DiffLine[] {
  const before = oldText === null ? [] : textLines(oldText);
  const after = textLines(newText);

  if (oldText === null) return after.map((text) => ({ sign: '+', text }));

  // Myers' shortest-edit-path algorithm. Its work scales with the size of the
  // edit rather than the size of the file, which suits full snapshots carrying
  // a handful of changed lines. The trace contains only edit diagonals, so
  // unchanged thousand-line spans do not make it quadratic.
  const max = before.length + after.length;
  const trace: Map<number, number>[] = [];
  let frontier = new Map<number, number>([[1, 0]]);

  for (let distance = 0; distance <= max; distance += 1) {
    trace.push(new Map(frontier));
    const next = new Map<number, number>();

    for (let diagonal = -distance; diagonal <= distance; diagonal += 2) {
      const down = frontier.get(diagonal + 1) ?? -1;
      const right = frontier.get(diagonal - 1) ?? -1;
      let x = diagonal === -distance || (diagonal !== distance && right < down) ? down : right + 1;
      let y = x - diagonal;

      while (x < before.length && y < after.length && before[x] === after[y]) {
        x += 1;
        y += 1;
      }
      next.set(diagonal, x);

      if (x >= before.length && y >= after.length) {
        const edits: DiffLine[] = [];
        let editX = before.length;
        let editY = after.length;

        for (let d = distance; d >= 0; d -= 1) {
          const previous = trace[d];
          const k = editX - editY;
          const previousDown = previous.get(k + 1) ?? -1;
          const previousRight = previous.get(k - 1) ?? -1;
          const previousK = k === -d || (k !== d && previousRight < previousDown) ? k + 1 : k - 1;
          const previousX = previous.get(previousK) ?? 0;
          const previousY = previousX - previousK;

          while (editX > previousX && editY > previousY) {
            editX -= 1;
            editY -= 1;
          }
          if (d === 0) break;
          if (editX === previousX) {
            edits.push({ sign: '+', text: after[editY - 1] });
            editY -= 1;
          } else {
            edits.push({ sign: '-', text: before[editX - 1] });
            editX -= 1;
          }
        }

        return edits.reverse();
      }
    }
    frontier = next;
  }

  return [];
}
