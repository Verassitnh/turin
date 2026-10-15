//! Port of upstream/packages/source-control-core/src (server + client halves).

pub mod ambient;
pub mod clock;
pub mod definition;
pub mod diff_file_revisions;
pub mod discovery;
pub mod driver;
pub mod host;
pub mod merge_message;
pub mod pull_request_checks;
pub mod pull_request_provider;
pub mod rate_limit;
pub mod redacted;
pub mod source_control_provider;

/// A boxed error that keeps a failure's raw cause for logs (upstream `Schema.Defect()` /
/// `cause: unknown`). Never put it on a transport.
pub type Cause = std::sync::Arc<dyn std::error::Error + Send + Sync + 'static>;

/// Wraps a plain message as a [`Cause`].
pub fn cause_from_message(message: impl Into<String>) -> Cause {
    #[derive(Debug)]
    struct Message(String);
    impl std::fmt::Display for Message {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for Message {}
    std::sync::Arc::new(Message(message.into()))
}
