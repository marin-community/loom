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
//   toolwait:MS[:title]  an in-progress execute tool that waits until MS or cancellation
//   image[:title]        a read tool whose result is an ACP image content block
//   toolfail[:title]     a tool_call that ends with status `failed`
//   plan                 a plan update with two entries
//   usage:USED:SIZE      a usage_update
//   task-notification:MS:TEXT
//                        after the prompt response, stream TEXT as an autonomous
//                        Claude background-task continuation and close it with
//                        Claude's cost-bearing task-notification usage marker
//   wait:MS              sleep MS ms (cancellable) — for queueing/interrupt/crash tests
//   /compact TEXT        model a provider-owned compaction for
//                        `FAKE_ACP_COMPACT_DELAY` ms
//   permission:NAME      a session/request_permission that BLOCKS the turn until the
//                        client answers (exercises both auto-answer and REST-answer)
//   resources            echo the names of supplied resource_link blocks
//   poison               make this adapter reject this and every later prompt;
//                        only a process restart clears the failure
//
// The turn ends with stop reason `end_turn`, or `cancelled` if a `session/cancel`
// arrived (or a pending permission was answered `cancelled`) while it ran.
// `FAKE_ACP_CANCEL_NOTICE=1` additionally emits the presentation-only
// "Conversation interrupted" agent chunk shortly after cancellation, matching
// adapters that can race that notice against the next prompt.

import { createInterface } from "node:readline";

const JSONRPC = "2.0";
let sessionId = null;
let sessionCounter = 0;
let cancelled = false;
const steeringSupported = process.env.FAKE_ACP_STEERING === "1";
const agentName = process.env.FAKE_ACP_AGENT_NAME || "fake-acp-agent";
const agentVersion = process.env.FAKE_ACP_AGENT_VERSION || "1.0.0";
const loadPermission = process.env.FAKE_ACP_LOAD_PERMISSION || "";
const fixedPermissionId = Number(process.env.FAKE_ACP_PERMISSION_ID || "0");
const fixedOutput =
  process.env.FAKE_ACP_FIXED_OUTPUT_B64 === undefined
    ? undefined
    : Buffer.from(process.env.FAKE_ACP_FIXED_OUTPUT_B64, "base64").toString("utf8");
const summaryOutput =
  process.env.FAKE_ACP_SUMMARY_OUTPUT_B64 === undefined
    ? undefined
    : Buffer.from(process.env.FAKE_ACP_SUMMARY_OUTPUT_B64, "base64").toString("utf8");
let promptActive = false;
const steeringQueue = [];
let promptEpoch = 0;
let promptResources = [];
let poisoned = false;
const pending = new Map(); // our request id -> resolver awaiting the client's response

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}
function respond(id, result) {
  send({ jsonrpc: JSONRPC, id, result });
}
function rejectMethod(id, method) {
  send({
    jsonrpc: JSONRPC,
    id,
    error: { code: -32601, message: `Method not found: ${method}` },
  });
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

const modeNames = {
  auto: "Auto",
  default: "Default",
  acceptEdits: "Accept edits",
  bypassPermissions: "Bypass permissions",
  plan: "Plan",
};
const MODES = (
  process.env.FAKE_ACP_MODES || "auto,default,acceptEdits,bypassPermissions,plan"
)
  .split(",")
  .filter(Boolean)
  .map((id) => ({ id, name: modeNames[id] || id }));
const modelValues = (process.env.FAKE_ACP_MODELS || "fake-fast,fake-deep").split(",");
let currentModel = modelValues[0];
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
      options: modelValues.map((value) => {
        const label = value.replaceAll("-", " ");
        return {
          value,
          name: label.charAt(0).toUpperCase() + label.slice(1),
        };
      }),
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
  const reqId = fixedPermissionId || 10000 + pending.size + Math.floor(Math.random() * 1000);
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

async function handleLoad(id, params) {
  sessionId = params.sessionId;
  // Replay a tiny scripted history as the spec's load notifications.
  notify({ sessionUpdate: "user_message_chunk", content: { type: "text", text: "earlier question" } });
  notify({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "earlier answer" } });
  // Some adapters replay an unanswered request and cannot complete load until
  // the client answers it. Tests opt into that setup-time handshake shape.
  if (loadPermission) await askPermission(loadPermission);
  respond(id, {
    modes: { currentModeId: currentMode, availableModes: MODES },
    configOptions: configOptions(),
  });
  advertiseCommands();
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
  } else if (tok.startsWith("toolwait:")) {
    const [, delay, ...titleParts] = tok.split(":");
    const toolCallId = "call-wait-" + Math.floor(Math.random() * 100000);
    notify({
      sessionUpdate: "tool_call",
      toolCallId,
      title: titleParts.join(":") || "Blocking tool",
      kind: "execute",
      status: "in_progress",
    });
    await sleepCancellable(Number(delay));
    if (!cancelled) {
      notify({ sessionUpdate: "tool_call_update", toolCallId, status: "completed" });
    }
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
  } else if (tok === "image" || tok.startsWith("image:")) {
    const title = tok.includes(":") ? tok.slice(tok.indexOf(":") + 1) : "Read screenshot.png";
    const toolCallId = "call-image-" + Math.floor(Math.random() * 100000);
    notify({
      sessionUpdate: "tool_call",
      toolCallId,
      title,
      kind: "read",
      status: "in_progress",
      content: [
        {
          type: "content",
          content: {
            type: "image",
            mimeType: "image/png",
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
            uri: "file:///tmp/screenshot.png",
          },
        },
      ],
      locations: [{ path: "/tmp/screenshot.png", line: 1 }],
    });
    await sleep(10);
    notify({
      sessionUpdate: "tool_call_update",
      toolCallId,
      status: "completed",
    });
  } else if (tok.startsWith("tool:")) {
    const rest = tok.slice(5);
    const [kind, title] = rest.split(":");
    const toolCallId = "call-" + kind + "-" + Math.floor(Math.random() * 100000);
    const content =
      kind === "edit"
        ? [
            {
              type: "diff",
              path: "/w/file.rs",
              oldText: "fn unchanged() {}\nold line\n// unchanged tail\n",
              newText: "fn unchanged() {}\nnew line\n// unchanged tail\n",
            },
          ]
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
  } else if (tok.startsWith("task-notification:")) {
    const [, delay, ...textParts] = tok.split(":");
    const text = textParts.join(":");
    setTimeout(async () => {
      const half = Math.ceil(text.length / 2);
      notify({
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: text.slice(0, half) },
      });
      await sleep(5);
      notify({
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: text.slice(half) },
      });
      notify({
        sessionUpdate: "usage_update",
        used: 42,
        size: 1000,
        cost: { amount: 0.01, currency: "USD" },
        _meta: { "_claude/origin": { kind: "task-notification" } },
      });
    }, Number(delay));
  } else if (tok.startsWith("wait:")) {
    await sleepCancellable(Number(tok.slice(5)));
  } else if (tok === "/compact" || tok.startsWith("/compact ")) {
    await sleepCancellable(Number(process.env.FAKE_ACP_COMPACT_DELAY || "0"));
  } else if (tok.startsWith("permission:")) {
    const outcome = await askPermission(tok.slice(11));
    if (!outcome || !outcome.outcome || outcome.outcome.outcome === "cancelled") {
      cancelled = true;
    }
  } else if (tok === "resources") {
    notify({
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: promptResources.map((r) => r.name).join(",") },
    });
  }
}

async function handlePrompt(id, params) {
  const epoch = ++promptEpoch;
  cancelled = false;
  promptActive = true;
  promptResources = (params.prompt || []).filter((b) => b.type === "resource_link");
  // The script is the prompt's first paragraph only. A real launch prompt
  // appends orientation prose (the entrance note, which echoes the session
  // title — i.e. the script itself) after a blank line; parsing past it would
  // run the scripted tokens twice.
  const text = (params.prompt || [])
    .map((b) => b.text || "")
    .join("")
    .split("\n\n")[0];
  if (text === "poison") poisoned = true;
  if (poisoned) {
    if (id !== null) {
      send({
        jsonrpc: JSONRPC,
        id,
        error: { code: -32000, message: "fake adapter is poisoned" },
      });
    }
    return;
  }
  if (fixedOutput !== undefined) {
    if (fixedOutput.length > 0) {
      notify({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: fixedOutput } });
    }
    if (id !== null) respond(id, { stopReason: "end_turn" });
    return;
  }
  if (summaryOutput !== undefined && text.startsWith("Summarize this coding session")) {
    await sleepCancellable(Number(process.env.FAKE_ACP_SUMMARY_DELAY || "0"));
    if (summaryOutput.length > 0) {
      notify({
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: summaryOutput },
      });
    }
    if (id !== null) respond(id, { stopReason: "end_turn" });
    return;
  }
  for (const tok of text.split("|")) {
    if (cancelled) break;
    if (tok.length === 0) continue;
    await runToken(tok);
    // An immediate cancel-and-restart can begin another prompt while this
    // handler is still unwinding its cancellable sleep.
    if (epoch !== promptEpoch) return;
    while (steeringQueue.length > 0 && !cancelled) {
      const steering = steeringQueue.shift();
      for (const steeringToken of steering.split("|")) {
        if (steeringToken.length > 0) await runToken(steeringToken);
      }
    }
  }
  promptActive = false;
  if (id !== null) respond(id, { stopReason: cancelled ? "cancelled" : "end_turn" });
}

function handleSteering(id, params) {
  const text = (params.prompt || []).map((block) => block.text || "").join("");
  if (!promptActive) {
    void handlePrompt(null, params);
    respond(id, { outcome: "startedNewTurn" });
    return;
  }
  steeringQueue.push(text);
  respond(id, { outcome: "injected" });
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
  // Integration tests use this to model an adapter process that stays alive
  // but never answers one setup stage — the production failure behind a create
  // request that otherwise waits forever.
  const ignoredMethods = new Set((process.env.FAKE_ACP_IGNORE_METHOD || "").split(",").filter(Boolean));
  if (ignoredMethods.has(msg.method)) return;
  switch (msg.method) {
    case "initialize":
      respond(msg.id, {
        protocolVersion: 1,
        agentCapabilities: { loadSession: true, promptCapabilities: {} },
        agentInfo: { name: agentName, version: agentVersion },
        ...(steeringSupported ? { _meta: { steering: { supported: true } } } : {}),
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
      void handleLoad(msg.id, msg.params);
      break;
    case "session/set_mode":
      currentMode = msg.params.modeId;
      notify({ sessionUpdate: "current_mode_update", currentModeId: msg.params.modeId });
      respond(msg.id, {});
      break;
    case "session/set_config_option":
      if (msg.params.configId === "model") {
        if (!modelValues.includes(msg.params.value)) {
          send({
            jsonrpc: JSONRPC,
            id: msg.id,
            error: { code: -32602, message: `Model ${msg.params.value} is not available` },
          });
          break;
        }
        currentModel = msg.params.value;
      }
      if (msg.params.configId === "thought_level") currentEffort = msg.params.value;
      if (msg.params.configId === "mode") currentMode = msg.params.value;
      if (msg.params.configId === "fast-mode") fastMode = msg.params.value;
      respond(msg.id, { configOptions: configOptions() });
      break;
    case "session/prompt":
      void handlePrompt(msg.id, msg.params);
      break;
    case "_session/steering":
      if (steeringSupported) handleSteering(msg.id, msg.params);
      else rejectMethod(msg.id, msg.method);
      break;
    case "session/cancel":
      cancelled = true;
      if (process.env.FAKE_ACP_CANCEL_NOTICE === "1") {
        setTimeout(() => {
          notify({
            sessionUpdate: "agent_message_chunk",
            content: { type: "text", text: "Conversation interrupted" },
          });
        }, 25);
      }
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
