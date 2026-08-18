use super::*;

const READ: &[&str] = &["loom/channels/read@v1"];
const WRITE: &[&str] = &["loom/channels/write@v1"];
const MESSAGE_KINDS: &[&str] = &["goal", "message", "status", "result", "system"];
const SEND_KINDS: &[&str] = &["message", "status", "result"];
const URGENCY_LEVELS: &[&str] = &["normal", "attention", "blocked"];

const fn channel_arg() -> ArgumentSpec {
    ArgumentSpec::string("channel")
        .minimum(1)
        .description("A visible channel id. Omit or pass 'self' for this session's channel.")
}

static LIST_ARGS: &[ArgumentSpec] = &[ArgumentSpec::boolean("archived").default_boolean(false)];
static SELECTOR_ARGS: &[ArgumentSpec] = &[channel_arg()];
static READ_ARGS: &[ArgumentSpec] = &[
    channel_arg(),
    ArgumentSpec::integer("after").minimum(0).default_integer(0),
    ArgumentSpec::integer("limit")
        .minimum(1)
        .maximum(crate::CHANNEL_MESSAGE_LIMIT_MAX as i64)
        .default_integer(100),
    ArgumentSpec::string_list("kinds")
        .choices(MESSAGE_KINDS)
        .unique_items(),
];
static SEND_ARGS: &[ArgumentSpec] = &[
    channel_arg(),
    ArgumentSpec::string("body")
        .minimum(1)
        .maximum(262_144)
        .required(),
    ArgumentSpec::string("kind")
        .choices(SEND_KINDS)
        .default_string("message"),
    ArgumentSpec::string("urgency")
        .choices(URGENCY_LEVELS)
        .default_string("normal"),
    ArgumentSpec::any("payload"),
    ArgumentSpec::string("reply_to").minimum(1),
    ArgumentSpec::string("idempotency_key")
        .minimum(1)
        .maximum(crate::CHANNEL_IDEMPOTENCY_KEY_MAX_LEN as i64),
];
static OPEN_ARGS: &[ArgumentSpec] = &[
    ArgumentSpec::string("name")
        .minimum(1)
        .maximum(120)
        .required(),
    ArgumentSpec::string("topic")
        .maximum(4096)
        .default_string(""),
];
static SUBSCRIBE_ARGS: &[ArgumentSpec] = &[
    channel_arg(),
    ArgumentSpec::string("mode")
        .choices(&["observe", "deliver"])
        .default_string("observe"),
    ArgumentSpec::string("session").minimum(1),
];
static ACK_ARGS: &[ArgumentSpec] = &[channel_arg(), ArgumentSpec::integer("seq").minimum(0)];
static WAIT_ARGS: &[ArgumentSpec] = &[
    channel_arg(),
    ArgumentSpec::integer("after").minimum(0),
    ArgumentSpec::string("kind").choices(MESSAGE_KINDS),
    ArgumentSpec::boolean("urgent").default_boolean(false),
    ArgumentSpec::integer("timeout")
        .minimum(1)
        .maximum(3600)
        .default_integer(1800),
];

static OPERATIONS: &[OperationSpec] = &[
    branch_operation!(
        "channels.list",
        "channels",
        "List visible durable channels and unread state.",
        SessionSelf,
        Read,
        "GET",
        "/api/channels",
        Some("loom channels list"),
        described_mcp(
            "loom_channel",
            "list",
            "List channels visible to this session, including unread state and binding summaries."
        ),
        READ,
        LIST_ARGS
    ),
    branch_operation!(
        "channels.get",
        "channels",
        "Inspect one channel and its delivery bindings.",
        SessionSelf,
        Read,
        "GET",
        "/api/channels/{channel}",
        Some("loom channels get [channel]"),
        described_mcp(
            "loom_channel",
            "get",
            "Get channel metadata and its server-owned delivery bindings."
        ),
        READ,
        SELECTOR_ARGS
    ),
    branch_operation!(
        "channels.messages.list",
        "channels",
        "Read channel messages and optionally advance the read marker.",
        SessionSelf,
        Read,
        "GET",
        "/api/channels/{channel}/messages",
        Some("loom channels read [--channel <id>]"),
        described_mcp(
            "loom_channel",
            "read",
            "Read an ordered channel stream without changing its read marker."
        ),
        READ,
        READ_ARGS
    ),
    branch_operation!(
        "channels.messages.create",
        "channels",
        "Append and deliver a durable channel message.",
        SessionSelf,
        Write,
        "POST",
        "/api/channels/{channel}/messages",
        Some("loom channels send <message>"),
        described_mcp(
            "loom_channel",
            "send",
            "Append one durable message and return its per-binding delivery receipts. Retrying with the same idempotency_key reuses the item and does not repeat a successful delivery."
        ),
        WRITE,
        SEND_ARGS
    ),
    branch_operation!(
        "channels.create",
        "channels",
        "Open a custom durable channel.",
        SessionSelf,
        Write,
        "POST",
        "/api/channels",
        Some("loom channels open <name>"),
        described_mcp(
            "loom_channel",
            "open",
            "Open a durable custom channel in this repository."
        ),
        WRITE,
        OPEN_ARGS
    ),
    branch_operation!(
        "channels.subscription.set",
        "channels",
        "Set how a session follows a channel.",
        SessionSelf,
        Write,
        "PUT",
        "/api/channels/{channel}/subscription",
        Some("loom channels subscribe"),
        described_mcp(
            "loom_channel",
            "subscribe",
            "Set observe or runtime-deliver mode for this session or a visible descendant session."
        ),
        WRITE,
        SUBSCRIBE_ARGS
    ),
    branch_operation!(
        "channels.read_marker.set",
        "channels",
        "Acknowledge a channel through a sequence number.",
        SessionSelf,
        Write,
        "PUT",
        "/api/channels/{channel}/read-marker",
        Some("loom channels ack"),
        described_mcp(
            "loom_channel",
            "ack",
            "Advance this session's read marker through a sequence, or through the latest item when seq is omitted."
        ),
        WRITE,
        ACK_ARGS
    ),
    branch_operation!(
        "channels.wait",
        "channels",
        "Wait for the next matching channel message.",
        SessionSelf,
        Read,
        "GET",
        "/api/channels/{channel}/messages",
        Some("loom channels wait"),
        described_mcp(
            "loom_channel",
            "wait",
            "Wait for the first matching channel item and return it with the new cursor."
        ),
        READ,
        WAIT_ARGS
    ),
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "channels",
        label: "Durable conversations",
        operations: OPERATIONS,
    }
}
