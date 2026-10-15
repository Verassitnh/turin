# QC wf_b2f290b9-219 — T00a base_schemas fix round

Reviewer: QC workflow (lead report).

## Lead report

Fix list for T00-foundation-fixes (re-QC of wf_6ce02f2f-38c). The six confirmed findings reduce to four after deduplication; three of the UtcDateTime findings were one issue. No confirmed finding says any of prior items 2, 4, 5, 9, 10, 11, 12, 14 or 15 was left unfixed.

1. **[low] crates/contracts/src/base_schemas.rs:194 (also :170 doc, :185-188 `from_datetime`): UtcDateTime parses a different set of strings than upstream and can write strings it cannot read back.**
   - **Problem:** `FromStr` only calls `DateTime::parse_from_rfc3339`. Upstream `DateTime.makeUnsafe` (effect 4.0.4) behaves differently:
     - Upstream accepts strings with no zone (appends "Z"), date-only strings, `+HHMM` offsets and minute-precision times. Turin rejects all of these.
     - Turin accepts `23:59:60Z` and re-encodes it as `...:60.000Z`, which upstream rejects.
     - Turin accepts lowercase `t`/`z`, which upstream rejects.
     - Years above 9999 encode as `+10000-...` or `+262142-...`. That breaks the PORTING promise of `YYYY-MM-DDTHH:MM:SS.sssZ`, and Turin cannot parse its own output or upstream's `+010000-...` form.
   - **Required fix:**
     - Reject `second == 60` when parsing.
     - Reject or clamp years outside 0..=9999 in `from_datetime`, `FromStr` and `Deserialize`, so every value encodes in the fixed format and round-trips.
     - Either add a fallback that treats strings with no zone as UTC (use upstream's `hasZone` regex; also accept date-only and `+HHMM`), or leave parsing as RFC 3339 only and tell the PM to add a Deviations row ("UtcDateTime accepts RFC 3339 only; upstream accepts anything JS Date parses").
     - Correct the doc comment at :170 to match whichever choice you make.
     - Add tests for `:60` rejection, the year bounds, and any fallback forms you add.

2. **[low] crates/contracts/src/base_schemas.rs:503-525 (also `forward_compatible_union_array` at :464-480): the forward-compatible array helpers cannot be used on `Option<Vec<T>>`.**
   - **Problem:** `serialize` takes `&[T]` and `deserialize` returns `Vec<T>`, so `with = "forward_compatible_array"` on an `Option<Vec<T>>` field fails to compile. A kept upstream field needs this: server.ts:264, `supportedRuntimeModes: Schema.optional(ForwardCompatibleArray(RuntimeMode))`, which kept RuntimePolicy.ts:78-82 consumes. PORTING forbids hand-rolling helpers, so a porter either writes their own or drops forward compatibility, and then one unknown mode fails the whole ServerProvider decode.
   - **Required fix:**
     - Add `forward_compatible_array::optional`. Its `serialize` takes `&Option<Vec<T>>`; its `deserialize` reads `Option<Vec<Value>>`, filters it, and returns `Some(filtered)`. Add the same to `forward_compatible_union_array`.
     - Add tests for an absent key, `[]`, and an array containing an unknown element.
     - Ask the PM to add the PORTING row: `optional/optionalKey(ForwardCompatibleArray(X))` -> `#[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_array::optional")] Option<Vec<X>>`.

3. **[low] crates/contracts/src/base_schemas.rs:178-179: the public field of UtcDateTime skips millisecond truncation.**
   - **Problem:** `pub struct UtcDateTime(pub DateTime<Utc>)` derives Eq, Ord and Hash on the raw DateTime. Writing `UtcDateTime(Utc::now())` or `UtcDateTime(x.0 + dur)` keeps nanoseconds. The value then does not equal its own round trip, and two values with identical wire text compare unequal or ordered. Upstream stores milliseconds only, so this cannot happen there. Nothing builds the value this way yet, but every later `*_at` port will use this type.
   - **Required fix:**
     - Make the field private.
     - Add `impl From<DateTime<Utc>> for UtcDateTime` (truncating) and an `as_datetime()` accessor.
     - Keep `now`, `from_datetime`, `FromStr` and `Deserialize` as the only other ways to build a value.

4. **[low] PORTING.md:65 (main; PM-owned, outside this branch's diff): the `ForwardCompatibleOptional(NullOr(X))` row leaves out `default` and `skip_serializing_if`.**
   - **Problem:** Applied literally, the row makes `{}` fail with "missing field" and makes an absent value encode as `null`. The helper itself and its doc comment at base_schemas.rs:575-578 are correct.
   - **Required fix:** No code change. Ask the PM to change the row to `Option<Option<X>>` + `#[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_optional_nullable")]`.

**Verdict:** PASS. There are no critical or high findings; the four items above are low severity.
