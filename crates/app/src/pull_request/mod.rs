//! The GitHub pull request views: port of upstream/apps/web/src/components/pullRequest.
//!
//! `*_logic` modules are the pure ports (with their upstream tests); the view modules render
//! from the view-model seam in `view_model` and never call the pull request service directly.

#![allow(dead_code)]

pub mod fixtures;
pub mod pull_request_list_logic;
