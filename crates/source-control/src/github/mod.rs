//! Port of upstream/packages/source-control-github/src (server + client).

pub mod api;
pub mod auth_status;
pub mod change_request_template;
pub mod conditional_checks;
pub mod credentials;
pub mod definition;
pub mod driver;
pub mod graphql;
pub mod pull_request_api;
pub mod pull_request_json;
pub mod pull_request_provider;
pub mod pull_requests;
pub mod quota;
pub mod repository_resolution;
pub mod source_control_provider;
pub mod stack_actions;
pub mod stack_rebase;
