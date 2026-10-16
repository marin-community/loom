import type { ToolCallPayload } from '../types';

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

export function toolCommand(tool: ToolCallPayload): string | null {
  if (tool.tool_kind !== 'execute') return null;
  if (typeof tool.input === 'string') return tool.input;
  const input = record(tool.input);
  const args = record(input?.arguments);
  const command = input?.command ?? input?.cmd ?? args?.command ?? args?.cmd;
  if (typeof command === 'string') return command;
  // ACP permits title-only execute calls; preserve those commands in the detail.
  return tool.title || null;
}

export function firstLine(text: string): string {
  return (
    text
      .split('\n')
      .map((line) => line.trim())
      .find(Boolean) ?? ''
  );
}

export function imageFilename(uri: string | null | undefined, title: string): string {
  return (uri || title).split('/').pop() || title;
}

export function toolLabel(tool: ToolCallPayload): string {
  if (tool.tool_kind === 'execute') {
    const description = record(tool.input)?.description;
    if (typeof description === 'string' && description.trim()) return firstLine(description);
    const command = toolCommand(tool)?.trim() ?? '';
    const [program, argument] = command.split(/\s+/);
    if (!program || !/^[\w./+-]+$/.test(program) || program.includes('=')) return 'Run command';
    const name = program.split('/').pop()!;
    const subcommand =
      ['git', 'cargo', 'npm', 'pnpm', 'yarn', 'uv', 'docker'].includes(name) &&
      argument &&
      /^[\w-]+$/.test(argument) &&
      !argument.startsWith('-')
        ? ` ${argument}`
        : '';
    return `${name}${subcommand}`;
  }
  if (tool.name === 'view_image' || tool.content.some((content) => content.type === 'image')) {
    const image = tool.content.find((content) => content.type === 'image');
    const path = image?.type === 'image' ? image.uri : null;
    const name = imageFilename(path || tool.locations[0]?.path, tool.title);
    return name ? `View ${name}` : 'View image';
  }
  return (
    firstLine(tool.title) ||
    (
      {
        read: 'Read file',
        edit: 'Edit files',
        search: 'Search files',
        fetch: 'Fetch resource',
        delete: 'Delete file',
        move: 'Move file',
        think: 'Review',
      } as Record<string, string>
    )[tool.tool_kind] ||
    'Tool action'
  );
}

export function toolFailure(tool: ToolCallPayload): string | null {
  if (tool.status !== 'failed') return null;
  const lines = tool.content
    .flatMap((content) => (content.type === 'text' ? content.text.split('\n') : []))
    .map((line) => line.trim())
    .filter(Boolean);
  const summary =
    lines.find((line) => /\b(error|fatal|exception|failed|denied|not found)\b/i.test(line)) ??
    lines[0] ??
    'The tool failed without an error message.';
  return summary.length > 240 ? `${summary.slice(0, 239)}…` : summary;
}
