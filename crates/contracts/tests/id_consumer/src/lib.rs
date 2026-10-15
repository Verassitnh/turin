//! Test fixture: declares ids with `turin_contracts::id!` from a crate that
//! has no direct serde/uuid dependency. See `tests/id_macro.rs`.

turin_contracts::id!(
    /// Doc comments and attributes pass through the macro.
    HookRunId,
    OtherId,
);
