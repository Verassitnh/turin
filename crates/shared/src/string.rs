//! Port of upstream/packages/shared/src/String.ts
//!
//! Upstream `String.ts` has no exports at the ported revision (reserved
//! file); this module is kept so the file map stays one-to-one. JS
//! string-semantics helpers live in [`crate::js_string`].

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder_matches_upstream_empty_module() {}
}
