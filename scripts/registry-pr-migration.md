Run from the merged PR worktree with its pre-merge approved head and old merge base:

```sh
python3 scripts/migrate_registry_pr.py "$base" "$approved_head"
```

Stage printed paths. Remove conflicted legacy subsystem JSON/records; remove old
diagnostic JSON/record only if the per-code directory exists. Resolve other mechanical
conflicts, refuse remaining unmerged paths, then run cheap and patch identity **before** push.
Compare the projected registry delta (`projection`/`rust_projection`), retaining file
identity/modes and nested hashes; ignore only entry order. Keep the normal check elsewhere.
The command never stages/deletes legacy files. It preserves main order, appends PR additions,
and is idempotent. Conflicting edits, unkeyed replacements, unsupported Rust, Rust/JSON
mismatch and header changes refuse before writes. Clean native diagnostics stay untouched.
Frozen #372/#409 inputs test both merge directions/phases, refusals and byte-identical reruns.
