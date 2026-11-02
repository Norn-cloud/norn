Run from the merged PR worktree: `python3 scripts/migrate_registry_pr.py "$base" "$approved_head"` (pre-merge approved head and old merge base).

Stage printed paths. Remove conflicted legacy subsystem JSON/records; remove old diagnostic JSON/record only if the per-code directory exists. Resolve other mechanical conflicts, refuse remaining unmerged paths, then run cheap and patch identity **before** push.
Compare the projected registry delta (`projection`/`rust_projection`), retaining file identity/modes and nested hashes; ignore only entry order. Keep the normal check elsewhere.
The command never stages/deletes legacy files. It preserves main order and is idempotent. Conflicts, unkeyed replacements, unsupported Rust, Rust/JSON mismatch and header changes refuse before writes; clean native diagnostics stay untouched.
Frozen #372/#409 inputs test both merge directions/phases, refusals and byte-identical reruns.
