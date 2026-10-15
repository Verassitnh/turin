//! Ports of the pieces of `@pierre/diffs@1.5.2` the app relies on: the data
//! model, the patch parser, partial-diff hydration and the row iterator.

pub mod hydrate_partial_diff;
pub mod iterate_over_diff;
pub mod parse_patch_files;
pub mod types;
