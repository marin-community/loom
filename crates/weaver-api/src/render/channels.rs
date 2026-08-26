//! Text rendering for durable channel operations.

use crate::dto::{ChannelMessageView, ChannelSubscriptionView, ChannelView};
use crate::operations::channels;
use crate::operations::{NoView, Render};

/// A channel row is one line, so its topic is trimmed to what fits beside it;
/// `channels get` prints the whole thing.
const TOPIC_WIDTH: usize = 100;

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

fn row(channel: &ChannelView) -> String {
    let urgent = if channel.unread_urgent_count > 0 {
        format!(" !{}", channel.unread_urgent_count)
    } else {
        String::new()
    };
    let unread = if channel.unread_count > 0 {
        format!(" +{}", channel.unread_count)
    } else {
        String::new()
    };
    let mut line = format!(
        "{}  {:<8} {}{}{}",
        channel.id, channel.kind, channel.name, unread, urgent
    );
    if !channel.topic.is_empty() {
        line.push_str(&format!("\n  {}", truncate(&channel.topic, TOPIC_WIDTH)));
    }
    line
}

fn detail(channel: &ChannelView) -> String {
    let mut lines = vec![
        format!("id:      {}", channel.id),
        format!("kind:    {}", channel.kind),
        format!("name:    {}", channel.name),
        format!("state:   {}", channel.state),
    ];
    if !channel.topic.is_empty() {
        lines.push(format!("topic:   {}", channel.topic));
    }
    lines.push("bindings:".to_string());
    if channel.bindings.is_empty() {
        lines.push("  (none)".to_string());
    }
    for binding in &channel.bindings {
        lines.push(format!(
            "  {}  {}  {}",
            binding.id, binding.kind, binding.label
        ));
    }
    lines.join("\n")
}

/// One message, with each binding's delivery outcome indented beneath it. A
/// delivery that failed only means something beside the message it carried.
fn message(message: &ChannelMessageView) -> String {
    let marker = match message.urgency.as_str() {
        "blocked" => "!!",
        "attention" => " !",
        _ => "  ",
    };
    let mut lines = vec![format!(
        "{:>4}{} {:<7} {}:{}  {}",
        message.seq, marker, message.kind, message.author_kind, message.author_id, message.body
    )];
    for delivery in &message.deliveries {
        let error = delivery
            .last_error
            .as_deref()
            .map(|error| format!(" — {error}"))
            .unwrap_or_default();
        lines.push(format!(
            "       delivery {} → {}{}",
            delivery.binding_id, delivery.state, error
        ));
    }
    lines.join("\n")
}

fn messages(items: &[ChannelMessageView]) -> String {
    if items.is_empty() {
        return "(no messages)".to_string();
    }
    items.iter().map(message).collect::<Vec<_>>().join("\n")
}

impl Render for channels::list::Op {
    fn text(output: &Vec<ChannelView>, _: &NoView) -> String {
        if output.is_empty() {
            return "(no channels)".to_string();
        }
        output.iter().map(row).collect::<Vec<_>>().join("\n")
    }
}

impl Render for channels::get::Op {
    fn text(output: &ChannelView, _: &NoView) -> String {
        detail(output)
    }
}

impl Render for channels::create::Op {
    fn text(output: &ChannelView, _: &NoView) -> String {
        format!("{}  {}", output.id, output.name)
    }
}

impl Render for channels::messages::list::Op {
    fn text(output: &Vec<ChannelMessageView>, _: &NoView) -> String {
        messages(output)
    }
}

impl Render for channels::messages::create::Op {
    fn text(output: &ChannelMessageView, _: &NoView) -> String {
        message(output)
    }
}

impl Render for channels::wait::Op {
    fn text(output: &ChannelMessageView, _: &channels::wait::View) -> String {
        message(output)
    }
}

impl Render for channels::subscription::set::Op {
    fn text(output: &ChannelSubscriptionView, _: &NoView) -> String {
        format!(
            "{}  {}:{}  {} through {}",
            output.channel_id, output.subject_kind, output.subject_id, output.mode, output.read_seq
        )
    }
}

impl Render for channels::read_marker::set::Op {
    fn text(output: &ChannelSubscriptionView, _: &NoView) -> String {
        format!("{} read through {}", output.channel_id, output.read_seq)
    }
}
