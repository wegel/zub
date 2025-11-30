//! transport layer for remote operations

pub mod local;
pub mod pull;
pub mod push;
pub mod ssh;

pub use local::{copy_objects, list_all_objects, ObjectSet, TransferStats};
pub use pull::{pull_local, pull_ssh, PullOptions, PullResult};
pub use push::{push_local, push_ssh, PushOptions, PushResult};
pub use ssh::SshConnection;
