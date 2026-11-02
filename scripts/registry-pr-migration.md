After merging main, run from the PR worktree, with the pre-merge approved head
and its old-layout merge base:

```sh
python3 scripts/migrate_registry_pr.py "$base" "$approved_head"
```

Stage the printed paths. `git rm` conflicted legacy subsystem JSON and its
provenance; remove old diagnostic JSON/record only when the per-code directory
exists. Resolve other approved mechanical conflicts, refuse remaining unmerged
paths, then run cheap and the approved-patch identity check **before** pushing.
For that check, compare the projected registry delta, not the raw old-file diff:
`projection`/`rust_projection` supply the approved side. Ignore only entry order;
retain file identity/modes and nested hashes. Keep the normal check elsewhere.

The command never stages or deletes legacy files. It preserves main's order,
appends PR additions, and is idempotent. Conflicting edits, unkeyed replacements,
unsupported Rust, Rust/JSON disagreement and header changes refuse before writes.
Native, clean diagnostic files stay untouched until that registry migrates.
Offline fixtures retain exact registry diffs and SHAs from approved #372/#409;
tests replay both merge directions and both diagnostic layouts.
