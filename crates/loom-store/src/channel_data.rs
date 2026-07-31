//! Shared channel record vocabulary.

pub const SESSION_KIND: &str = "session";
pub const CUSTOM_KIND: &str = "custom";
pub const OPEN_STATE: &str = "open";
pub const ARCHIVED_STATE: &str = "archived";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Goal,
    Message,
    Status,
    Result,
    System,
}

impl MessageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::Message => weaver_api::CHANNEL_DEFAULT_MESSAGE_KIND,
            Self::Status => "status",
            Self::Result => "result",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "goal" => Some(Self::Goal),
            "message" => Some(Self::Message),
            "status" => Some(Self::Status),
            "result" => Some(Self::Result),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionMode {
    Observe,
    Deliver,
}

impl SubscriptionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observe => weaver_api::CHANNEL_DEFAULT_SUBSCRIPTION_MODE,
            Self::Deliver => "deliver",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "observe" => Some(Self::Observe),
            "deliver" => Some(Self::Deliver),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Normal,
    Attention,
    Blocked,
}

impl Urgency {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => weaver_api::CHANNEL_DEFAULT_URGENCY,
            Self::Attention => "attention",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "attention" => Some(Self::Attention),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }

    pub fn from_status_level(value: &str) -> Self {
        match value {
            "blocked" => Self::Blocked,
            "attention" => Self::Attention,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectKind {
    Session,
    User,
    Automation,
    System,
}

impl SubjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::User => "user",
            Self::Automation => "automation",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "session" => Some(Self::Session),
            "user" => Some(Self::User),
            "automation" => Some(Self::Automation),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Subject {
    pub kind: SubjectKind,
    pub id: String,
}

impl Subject {
    pub fn new(kind: SubjectKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}
