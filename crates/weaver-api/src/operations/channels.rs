//! Durable conversation channels.
//!
//! Session channels, custom channels, their messages, subscriptions, and read
//! markers.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod archive {
    use super::prelude::*;

    /// Archive a custom channel.
    ///
    /// Only a custom channel: a session's own channel follows the session's
    /// lifecycle, and archiving it out from under the session is refused. Who may
    /// archive is narrower than who may reach the channel, so the handler still
    /// checks it — a non-human credential may archive only what it opened.
    #[operation(id = "channels.archive", actor = SessionSelf, scope = Branch, risk = Destructive,
                grants = ["loom/channels/write@v1"], cli = "channels archive")]
    pub struct Input {
        /// A visible channel id.
        #[operand(positional)]
        pub channel: String,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ChannelArchiveResult;
}

pub mod bindings {
    //! A channel's external delivery bindings: subscribed session inboxes plus,
    //! if wired, the originating Slack thread.
    pub(super) use super::prelude;
    pub mod list {
        use super::prelude::*;

        /// List a channel's external delivery bindings: subscribed session inboxes,
        /// plus the originating Slack thread if the branch is wired to one.
        #[operation(id = "channels.bindings.list", actor = SessionSelf, scope = Branch, risk = Read,
                    grants = ["loom/channels/read@v1"])]
        pub struct Input {
            /// A visible channel id. Empty means this session's own channel,
            /// resolved server-side.
            #[operand(default = String::new())]
            pub channel: String,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = Vec<ChannelBindingView>;
    }
}

pub mod create {
    use super::prelude::*;

    /// Open a custom durable channel.
    ///
    /// Scoped to a repository (humans launching from the dashboard specify no branch).
    /// A session's opening branch is recorded for provenance, not scope.
    #[operation(id = "channels.create", actor = SessionSelf, scope = Repository, risk = Write,
                grants = ["loom/channels/write@v1"], cli = "channels open", render = custom)]
    pub struct Input {
        /// The new channel's name.
        #[operand(positional)]
        #[schemars(length(min = 1, max = 120))]
        pub name: String,
        /// Optional topic description.
        #[operand(default = String::new())]
        #[schemars(length(max = 4096))]
        pub topic: String,
        /// The repository the channel belongs to. Resolved from the calling
        /// session when it has one.
        #[operand(context)]
        pub repo_root: String,
        /// The branch that opened the channel, for provenance. Resolved from the
        /// calling session; a human launch leaves it unset.
        #[operand(context)]
        pub branch: Option<String>,
    }

    pub type Output = ChannelView;
}

pub mod get {
    use super::prelude::*;

    /// Inspect one channel and its delivery bindings.
    #[operation(id = "channels.get", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/channels/read@v1"], cli = "channels get", render = custom)]
    pub struct Input {
        /// A visible channel id. Empty means this session's own channel,
        /// resolved server-side.
        #[operand(positional, default = String::new())]
        pub channel: String,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ChannelView;
}

pub mod list {
    use super::prelude::*;

    /// List visible durable channels and their unread state.
    #[operation(id = "channels.list", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/channels/read@v1"], cli = "channels list", cli_alias = "ls",
                render = custom)]
    pub struct Input {
        /// Include archived channels.
        // Spelled `--all` because that is the word the command line has always
        // taken; the operand the other transports send is `archived`.
        #[operand(default = false, long = "all")]
        pub archived: bool,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = Vec<ChannelView>;
}

pub mod messages {
    //! Items within one durable channel.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Append and deliver a durable channel message.
        ///
        /// Idempotent on `idempotency_key`.
        #[operation(id = "channels.messages.create", actor = SessionSelf, scope = Branch,
                    risk = Write, grants = ["loom/channels/write@v1"], cli = "channels send",
                    render = custom)]
        pub struct Input {
            /// A visible channel id. Empty means this session's own channel,
            /// resolved server-side.
            #[operand(default = String::new())]
            pub channel: String,
            /// The message body.
            #[operand(positional)]
            #[schemars(length(min = 1, max = 262_144))]
            pub body: String,
            /// `message`, `status`, or `result`.
            #[operand(default = String::from("message"))]
            #[schemars(extend("enum" = ["message", "status", "result"]))]
            pub kind: String,
            /// `normal`, `attention`, or `blocked`.
            #[operand(default = String::from("normal"))]
            #[schemars(extend("enum" = ["normal", "attention", "blocked"]))]
            pub urgency: String,
            /// Arbitrary structured payload alongside the body.
            #[operand(json, default = serde_json::json!({}))]
            pub payload: serde_json::Value,
            /// Reply to an existing message in this channel.
            #[schemars(length(min = 1))]
            pub reply_to: Option<String>,
            /// Retry-safe key scoped to the channel.
            #[schemars(length(min = 1, max = 255))]
            pub idempotency_key: Option<String>,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = ChannelMessageView;
    }

    pub mod list {
        use super::prelude::*;

        /// Read a channel's message history, advancing the read marker unless
        /// peeking.
        #[operation(id = "channels.messages.list", actor = SessionSelf, scope = Branch, risk = Read,
                    grants = ["loom/channels/read@v1"], cli = "channels read", render = custom)]
        pub struct Input {
            /// A visible channel id. Empty means this session's own channel,
            /// resolved server-side.
            #[operand(default = String::new())]
            pub channel: String,
            /// Only return items after this sequence number.
            #[operand(default = 0)]
            #[schemars(range(min = 0))]
            pub after: i64,
            /// Maximum number of items to return.
            #[operand(default = 100)]
            #[schemars(range(min = 1, max = 500))]
            pub limit: i64,
            /// Restrict to these message kinds (`goal`, `message`, `status`,
            /// `result`, `system`).
            #[schemars(extend("uniqueItems" = true))]
            #[schemars(extend("items" = {
                "type": "string",
                "enum": ["goal", "message", "status", "result", "system"]
            }))]
            pub kinds: Vec<String>,
            /// Read without advancing this session's read marker.
            #[operand(default = false)]
            pub peek: bool,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = Vec<ChannelMessageView>;
    }
}

pub mod read_marker {
    //! A session's read marker on a channel.
    pub(super) use super::prelude;
    pub mod set {
        use super::prelude::*;

        /// Acknowledge a channel through a sequence number.
        #[operation(id = "channels.read_marker.set", actor = SessionSelf, scope = Branch,
                    risk = Write, grants = ["loom/channels/write@v1"], cli = "channels ack",
                    render = custom)]
        pub struct Input {
            /// A visible channel id. Empty means this session's own channel,
            /// resolved server-side.
            #[operand(default = String::new())]
            pub channel: String,
            /// Mark read through this sequence; omission advances through the
            /// latest message.
            #[schemars(range(min = 0))]
            pub seq: Option<i64>,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = ChannelSubscriptionView;
    }
}

pub mod subscription {
    //! A session's subscription to a channel.
    pub(super) use super::prelude;
    pub mod set {
        use super::prelude::*;

        /// Set how a session follows a channel.
        #[operation(id = "channels.subscription.set", actor = SessionSelf, scope = Branch,
                    risk = Write, grants = ["loom/channels/write@v1"], cli = "channels subscribe",
                    render = custom)]
        pub struct Input {
            /// A visible channel id. Empty means this session's own channel,
            /// resolved server-side.
            #[operand(default = String::new())]
            pub channel: String,
            /// `observe` or `deliver`.
            #[operand(default = String::from("observe"))]
            #[schemars(extend("enum" = ["observe", "deliver"]))]
            pub mode: String,
            /// Subscribe this descendant session instead of the caller.
            #[schemars(length(min = 1))]
            pub session: Option<String>,
            #[operand(context)]
            pub branch: String,
        }

        pub type Output = ChannelSubscriptionView;
    }
}

pub mod wait {
    use super::prelude::*;

    /// Wait for the next matching channel message.
    #[operation(id = "channels.wait", actor = SessionSelf, scope = Branch, risk = Read,
                grants = ["loom/channels/read@v1"], cli = "channels wait", view = View,
                render = custom)]
    pub struct Input {
        /// A visible channel id. Empty means this session's own channel,
        /// resolved server-side.
        #[operand(default = String::new())]
        pub channel: String,
        /// Wait for items after this sequence; omission starts from the
        /// channel's latest known message.
        #[schemars(range(min = 0))]
        pub after: Option<i64>,
        /// Wake only for this message kind, e.g. `result`.
        #[schemars(extend("enum" = ::serde_json::json!(["goal", "message", "status", "result", "system", null])))]
        pub kind: Option<String>,
        /// Wake only for `attention` or `blocked` urgency.
        #[operand(default = false)]
        pub urgent: bool,
        /// Seconds to wait before giving up.
        #[operand(default = 1800)]
        #[schemars(range(min = 1, max = 3600))]
        pub timeout: i64,
        #[operand(context)]
        pub branch: String,
    }

    pub type Output = ChannelMessageView;

    /// CLI-only flags that never cross the wire.
    #[derive(Debug, Clone, Default, Deserialize, View)]
    pub struct View {
        /// Seconds between polls while waiting.
        #[operand(default = 2)]
        pub interval: i64,
    }
}

static OPERATIONS: &[&OperationSpec] = &[
    list::SPEC,
    get::SPEC,
    messages::list::SPEC,
    messages::create::SPEC,
    create::SPEC,
    archive::SPEC,
    subscription::set::SPEC,
    read_marker::set::SPEC,
    wait::SPEC,
    bindings::list::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "channels",
        label: "Durable conversations",
        operations: OPERATIONS,
    }
}
