#!/usr/bin/env node
// A scripted fake ACP agent for the loom integration suite.
//
// Speaks newline-delimited JSON-RPC 2.0 on stdio, exactly as `claude-agent-acp`
// / `codex-acp` do. It answers `initialize` (advertising `loadSession`),
// `session/new`, `session/load` (replays a tiny scripted history), and
// `session/prompt`. A prompt's *text* is a tiny `|`-separated script that drives
// which `session/update` notifications the turn emits, so a test gets
// deterministic ACP shapes with no real model:
//
//   say:hello            two agent_message_chunks that consolidate to "hello"
//   think:reasoning      one agent_thought_chunk
//   echo:text            a user_message_chunk — the adapter re-streaming a user
//                        turn (what claude does after /compact); must NOT re-journal
//   tool:edit[:title]    a tool_call (in_progress) then tool_call_update (completed);
//                        an `edit` kind carries a diff, others a text content block
//   toolfail[:title]     a tool_call that ends with status `failed`
//   plan                 a plan update with two entries
//   usage:USED:SIZE      a usage_update
//   wait:MS              sleep MS ms (cancellable) — for queueing/interrupt/crash tests
//   permission:NAME      a session/request_permission that BLOCKS the turn until the
//                        client answers (exercises both auto-answer and REST-answer)
//
// The turn ends with stop reason `end_turn`, or `cancelled` if a `session/cancel`
// arrived (or a pending permission was answered `cancelled`) while it ran.

import { createInterface } from "node:readline";

const JSONRPC = "2.0";
let sessionId = null;
let sessionCounter = 0;
let cancelled = false;
const pending = new Map(); // our request id -> resolver awaiting the client's response

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}
function respond(id, result) {
  send({ jsonrpc: JSONRPC, id, result });
}
function notify(update) {
  send({ jsonrpc: JSONRPC, method: "session/update", params: { sessionId, update } });
}
function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}
async function sleepCancellable(ms) {
  const step = 20;
  let elapsed = 0;
  while (elapsed < ms && !cancelled) {
    await sleep(Math.min(step, ms - elapsed));
    elapsed += step;
  }
}

const MODES = [
  { id: "default", name: "Default" },
  { id: "acceptEdits", name: "Accept edits" },
  { id: "bypassPermissions", name: "Bypass permissions" },
  { id: "plan", name: "Plan" },
];
let currentModel = "fake-fast";
let currentEffort = "medium";
let currentMode = "default";
let fastMode = false;

function configOptions() {
  return [
    {
      id: "mode",
      name: "Mode",
      description: "Approval and sandboxing preset for the session",
      category: "mode",
      type: "select",
      currentValue: currentMode,
      options: MODES.map((mode) => ({ value: mode.id, name: mode.name })),
    },
    {
      id: "model",
      name: "Model",
      description: "Model used for the next turn",
      category: "model",
      type: "select",
      currentValue: currentModel,
      options: [
        { value: "fake-fast", name: "Fake fast" },
        { value: "fake-deep", name: "Fake deep" },
      ],
    },
    {
      id: "thought_level",
      name: "Reasoning",
      description: "Reasoning effort",
      category: "thought_level",
      type: "select",
      currentValue: currentEffort,
      options: [
        { value: "low", name: "Low" },
        { value: "medium", name: "Medium" },
        { value: "high", name: "High" },
      ],
    },
    {
      id: "fast-mode",
      name: "Fast",
      description: "Use the faster service tier",
      type: "boolean",
      currentValue: fastMode,
    },
  ];
}

function advertiseCommands() {
  notify({
    sessionUpdate: "available_commands_update",
    availableCommands: [
      { name: "resume", description: "Resume a previous conversation" },
      {
        name: "review",
        description: "Review the current changes",
        input: { hint: "instructions" },
      },
    ],
  });
}

function askPermission(name) {
  const reqId = 10000 + pending.size + Math.floor(Math.random() * 1000);
  const toolCallId = "perm-tool-" + reqId;
  const toolCall = { toolCallId, title: "Edit " + name, kind: "edit", status: "pending" };
  const options = [
    { optionId: "allow-once", name: "Allow once", kind: "allow_once" },
    { optionId: "allow-always", name: "Always allow", kind: "allow_always" },
    { optionId: "reject", name: "Reject", kind: "reject_once" },
  ];
  const p = new Promise((resolve) => pending.set(reqId, resolve));
  send({
    jsonrpc: JSONRPC,
    id: reqId,
    method: "session/request_permission",
    params: { sessionId, toolCall, options },
  });
  return p;
}

async function runToken(tok) {
  if (tok.startsWith("say:")) {
    const text = tok.slice(4);
    const half = Math.ceil(text.length / 2);
    notify({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: text.slice(0, half) } });
    await sleep(5);
    notify({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: text.slice(half) } });
  } else if (tok.startsWith("think:")) {
    notify({ sessionUpdate: "agent_thought_chunk", content: { type: "text", text: tok.slice(6) } });
  } else if (tok.startsWith("echo:")) {
    notify({ sessionUpdate: "user_message_chunk", content: { type: "text", text: tok.slice(5) } });
  } else if (tok.startsWith("toolfail")) {
    const title = tok.includes(":") ? tok.slice(tok.indexOf(":") + 1) : "Failing tool";
    const toolCallId = "call-fail-" + Math.floor(Math.random() * 100000);
    notify({
      sessionUpdate: "tool_call",
      toolCallId,
      title,
      kind: "execute",
      status: "in_progress",
      content: [{ type: "content", content: { type: "text", text: "exit 1: boom" } }],
    });
    await sleep(10);
    notify({ sessionUpdate: "tool_call_update", toolCallId, status: "failed" });
  } else if (tok.startsWith("tool:")) {
    const rest = tok.slice(5);
    const [kind, title] = rest.split(":");
    const toolCallId = "call-" + kind + "-" + Math.floor(Math.random() * 100000);
    const content =
      kind === "edit"
        ? [{ type: "diff", path: "/w/file.rs", oldText: "old line\n", newText: "new line\n" }]
        : [{ type: "content", content: { type: "text", text: "done" } }];
    notify({
      sessionUpdate: "tool_call",
      toolCallId,
      title: title || "Tool " + kind,
      kind,
      status: "in_progress",
      content,
      locations: [{ path: "/w/file.rs", line: 1 }],
    });
    await sleep(10);
    notify({ sessionUpdate: "tool_call_update", toolCallId, status: "completed" });
  } else if (tok === "plan") {
    notify({
      sessionUpdate: "plan",
      entries: [
        { content: "first step", priority: "high", status: "completed" },
        { content: "second step", priority: "medium", status: "in_progress" },
      ],
    });
  } else if (tok.startsWith("usage:")) {
    const [, used, size] = tok.split(":");
    notify({ sessionUpdate: "usage_update", used: Number(used), size: Number(size) });
  } else if (tok.startsWith("wait:")) {
    await sleepCancellable(Number(tok.slice(5)));
  } else if (tok.startsWith("permission:")) {
    const outcome = await askPermission(tok.slice(11));
    if (!outcome || !outcome.outcome || outcome.outcome.outcome === "cancelled") {
      cancelled = true;
    }
  }
}

async function handlePrompt(id, params) {
  cancelled = false;
  // The script is the prompt's first paragraph only. A real launch prompt
  // appends orientation prose (the entrance note, which echoes the session
  // title — i.e. the script itself) after a blank line; parsing past it would
  // run the scripted tokens twice.
  const text = (params.prompt || [])
    .map((b) => b.text || "")
    .join("")
    .split("\n\n")[0];
  for (const tok of text.split("|")) {
    if (cancelled) break;
    if (tok.length === 0) continue;
    await runToken(tok);
  }
  respond(id, { stopReason: cancelled ? "cancelled" : "end_turn" });
}

function handleMessage(msg) {
  // A response to one of our requests (permission)?
  if (msg.id !== undefined && msg.method === undefined) {
    const resolver = pending.get(msg.id);
    if (resolver) {
      pending.delete(msg.id);
      resolver(msg.result || {});
    }
    return;
  }
  switch (msg.method) {
    case "initialize":
      respond(msg.id, {
        protocolVersion: 1,
        agentCapabilities: { loadSession: true, promptCapabilities: {} },
      });
      break;
    case "session/new":
      sessionId = "fake-session-" + ++sessionCounter;
      respond(msg.id, {
        sessionId,
        modes: { currentModeId: currentMode, availableModes: MODES },
        configOptions: configOptions(),
      });
      advertiseCommands();
      break;
    case "session/load":
      sessionId = msg.params.sessionId;
      // Replay a tiny scripted history as the spec's load notifications.
      notify({ sessionUpdate: "user_message_chunk", content: { type: "text", text: "earlier question" } });
      notify({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "earlier answer" } });
      respond(msg.id, {
        modes: { currentModeId: currentMode, availableModes: MODES },
        configOptions: configOptions(),
      });
      advertiseCommands();
      break;
    case "session/set_mode":
      currentMode = msg.params.modeId;
      notify({ sessionUpdate: "current_mode_update", currentModeId: msg.params.modeId });
      respond(msg.id, {});
      break;
    case "session/set_config_option":
      if (msg.params.configId === "model") currentModel = msg.params.value;
      if (msg.params.configId === "thought_level") currentEffort = msg.params.value;
      if (msg.params.configId === "mode") currentMode = msg.params.value;
      if (msg.params.configId === "fast-mode") fastMode = msg.params.value;
      respond(msg.id, { configOptions: configOptions() });
      break;
    case "session/prompt":
      handlePrompt(msg.id, msg.params);
      break;
    case "session/cancel":
      cancelled = true;
      break;
    default:
      // Unknown request: answer with an empty result so nothing hangs.
      if (msg.id !== undefined) respond(msg.id, {});
      break;
  }
}

const rl = createInterface({ input: process.stdin });
rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let msg;
  try {
    msg = JSON.parse(trimmed);
  } catch {
    return;
  }
  handleMessage(msg);
});
rl.on("close", () => process.exit(0));
