import test from 'node:test';
import assert from 'node:assert/strict';
import { toolCommand, toolFailure, toolLabel } from './toolPresentation.ts';

function tool(fields = {}) {
  return {
    tool_call_id: 'call',
    title: '',
    tool_kind: 'execute',
    status: 'completed',
    content: [],
    locations: [],
    ...fields,
  };
}

test('structured commands remain exact while labels use the description', () => {
  const command = "cat <<'EOF'\n  keep indentation\nEOF\n";
  const call = tool({
    title: 'Write the example',
    input: { command, description: 'Write an example\nMore context' },
  });
  assert.equal(toolCommand(call), command);
  assert.equal(toolLabel(call), 'Write an example');
});

test('command shapes from adapters and title-only calls preserve the complete command', () => {
  const command = 'cargo test\nprintf done\n';
  for (const fields of [
    { input: { cmd: command } },
    { input: { arguments: { command } } },
    { input: command },
    { title: command },
  ]) {
    assert.equal(toolCommand(tool(fields)), command);
  }
});

test('long shell bodies do not enter the action label', () => {
  const call = tool({
    input: { command: 'uv run --no-sync python - <<\'PY\'\nprint("large body")\nPY' },
  });
  assert.equal(toolLabel(call), 'uv run');
  assert.ok(!toolLabel(call).includes('large body'));
  assert.equal(toolLabel(tool({ title: 'KEY=value cargo test' })), 'Run command');
});

test('failure summaries select the error without turning successful stderr into a failure', () => {
  const content = [
    {
      type: 'text',
      text: 'Checking dependencies\nBuilding application\nerror: missing package\nfull diagnostic follows',
    },
  ];
  assert.equal(toolFailure(tool({ status: 'failed', content })), 'error: missing package');
  assert.equal(toolFailure(tool({ content })), null);
});

test('read and image calls retain their identity without inventing a shell command', () => {
  const call = tool({
    tool_kind: 'read',
    name: 'view_image',
    title: 'View Image /tmp/atlas.png',
    locations: [{ path: '/tmp/atlas.png', line: null }],
  });
  assert.equal(toolCommand(call), null);
  assert.equal(toolLabel(call), 'View atlas.png');
});
