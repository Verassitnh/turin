//! `id!` must work from a crate that depends on neither `serde` nor `uuid`.
//! The fixture crate compiling is the proof; this checks the generated impls.

use turin_contracts_id_consumer::{HookRunId, OtherId};

#[test]
fn id_macro_works_without_direct_serde_or_uuid_dependencies() {
    let id = HookRunId::new("\u{feff} run-1 ").unwrap();
    assert_eq!(id.as_str(), "run-1");
    assert_eq!(serde_json::to_string(&id).unwrap(), "\"run-1\"");
    assert_eq!(serde_json::from_str::<HookRunId>("\" run-1\"").unwrap(), id);
    assert!(serde_json::from_str::<HookRunId>("\"  \"").is_err());
    assert!(HookRunId::new("").is_err());
    assert_ne!(HookRunId::random(), HookRunId::random());
    assert!(!OtherId::random().as_str().is_empty());
}
