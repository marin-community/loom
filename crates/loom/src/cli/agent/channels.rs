//! `loom channels` — session channels and the custom ones opened beside them.

use anyhow::{bail, Result};
use clap::Subcommand;
use serde_json::json;

use weaver_api::operations::{branches, channels};

use super::{branch_key, channel_key, client, render};

// - `open` and `send` join their trailing words into one value, so
//   `loom channels send ready for review` works unquoted.
// - `wait` polls from the client, so it can acknowledge what it scans and print
//   every matching message rather than only the first.
#[derive(Subcommand)]
pub enum ChannelCmd {
    /// Open a custom channel alongside the current session channel.
    Open {
        name: Vec<String>,
        #[arg(long, default_value = "")]
        topic: String,
    },
    /// Send a message; on a session channel this also delivers it to the agent.
    Send {
        text: Vec<String>,
        /// Channel id; defaults to this session's channel.
        #[arg(long)]
        channel: Option<String>,
        #[arg(long, default_value = weaver_api::CHANNEL_DEFAULT_MESSAGE_KIND)]
        kind: String,
        #[arg(long, default_value = weaver_api::CHANNEL_DEFAULT_URGENCY)]
        urgency: String,
        /// Retry-safe key scoped to the channel.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Wait for the next channel message.
    Wait {
        /// Channel id; defaults to this session's channel.
        #[arg(long)]
        channel: Option<String>,
        /// Begin after this sequence; omission begins after the current latest.
        #[arg(long)]
        after: Option<i64>,
        /// Wake only for this message kind (for example `result`).
        #[arg(long)]
        kind: Option<String>,
        /// Wake only for attention/blocked urgency.
        #[arg(long)]
        urgent: bool,
        #[arg(long, default_value = "1800")]
        timeout: u64,
        #[arg(long, default_value = "2")]
        interval: u64,
    },
}

pub async fn run(cmd: ChannelCmd) -> Result<()> {
    cmd_channel(cmd).await
}

async fn cmd_channel(cmd: ChannelCmd) -> Result<()> {
    let client = client();
    match cmd {
        ChannelCmd::Open { name, topic } => {
            let name = name.join(" ");
            if name.trim().is_empty() {
                bail!("channel name is required");
            }
            let repo_root = match branch_key() {
                Ok(key) => Some(
                    client
                        .invoke::<branches::get::Op>(&branches::get::Input {
                            branch: key.to_string(),
                        })
                        .await?
                        .repo_root,
                ),
                Err(_) => None,
            };
            let channel = client
                .invoke::<channels::create::Op>(&channels::create::Input {
                    name: name.clone(),
                    topic: topic.clone(),
                    repo_root: repo_root.clone().unwrap_or_default(),
                    branch: None,
                })
                .await?;
            println!("{}", render::<channels::create::Op>(&channel));
        }
        ChannelCmd::Send {
            text,
            channel,
            kind,
            urgency,
            idempotency_key,
        } => {
            let id = channel_key(channel)?;
            let body = text.join(" ");
            if body.trim().is_empty() {
                bail!("message text is required");
            }
            let message = client
                .invoke::<channels::messages::create::Op>(&channels::messages::create::Input {
                    channel: id.to_string(),
                    body: body.clone(),
                    kind: kind.clone(),
                    urgency: urgency.clone(),
                    payload: (json!({})).clone(),
                    reply_to: None.clone(),
                    idempotency_key: idempotency_key.clone(),
                    branch: String::new(),
                })
                .await?;
            println!("{}", render::<channels::messages::create::Op>(&message));
        }
        ChannelCmd::Wait {
            channel,
            after,
            kind,
            urgent,
            timeout,
            interval,
        } => {
            let id = channel_key(channel)?;
            let mut cursor = match after {
                Some(seq) => seq.max(0),
                None => client
                    .invoke::<channels::get::Op>(&channels::get::Input {
                        channel: id.to_string(),
                        branch: String::new(),
                    })
                    .await?
                    .last_message
                    .map(|message| message.seq)
                    .unwrap_or(0),
            };
            let deadline = (timeout > 0)
                .then(|| std::time::Instant::now() + std::time::Duration::from_secs(timeout));
            loop {
                let messages = client.channel_messages(&id, cursor).await?;
                if let Some(last) = messages.last() {
                    cursor = last.seq;
                    client
                        .invoke::<channels::read_marker::set::Op>(
                            &channels::read_marker::set::Input {
                                channel: id.to_string(),
                                seq: Some(cursor),
                                branch: String::new(),
                            },
                        )
                        .await?;
                    let matching = messages
                        .iter()
                        .filter(|message| {
                            kind.as_deref().is_none_or(|kind| message.kind == kind)
                                && (!urgent
                                    || matches!(message.urgency.as_str(), "attention" | "blocked"))
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    if !matching.is_empty() {
                        println!("{}", render::<channels::messages::list::Op>(&matching));
                        return Ok(());
                    }
                }
                if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
                    bail!("timed out waiting for channel {id}");
                }
                let nap = std::time::Duration::from_secs(interval.max(1));
                tokio::time::sleep(match deadline {
                    Some(end) => nap.min(end.saturating_duration_since(std::time::Instant::now())),
                    None => nap,
                })
                .await;
            }
        }
    }
    Ok(())
}
