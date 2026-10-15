# Pinned Codex app-server schema

Source: `openai/codex@687a119f0fcaace47e1f1abcc77cec6c813fd6da`, the exact ref in upstream `effect-codex-app-server/scripts/generate.ts`. The Codex workspace version at this commit is **0.159.0**. The locally installed `codex-cli` was 0.162.1 during this port; it was not used to generate these types.

`json/` contains the JSON Schema files used by the upstream generator, and `typescript/` contains the four request/notification method declarations. To refresh the pin, update `REF` in `scripts/fetch_pinned.py`, fetch, then run `python3 crates/codex-app-server/scripts/generate.py`. The generator writes the aggregate schema and checked-in method, namespace, and serde wrapper files to `src/generated/`. Normal crate builds and tests do not fetch anything.
