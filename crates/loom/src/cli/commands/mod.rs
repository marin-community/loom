//! Hand-written commands over declared operations.
//!
//! Each of these marshals flags into an operation's input and formats its
//! output by hand. A command belongs here only while it does something one
//! declaration cannot — sequencing two operations, joining trailing argv
//! words, or reading a local file. When that reason goes away the command goes
//! with it, and the declaration serves the invocation on its own.

pub mod layout;
pub mod mcps;
pub mod permissions;
pub mod profiles;
pub mod review;
pub mod sessions;
pub mod watches;
