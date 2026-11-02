After merging the migrated main into an old-layout PR, retain the pre-merge PR
head and its merge base with main. Run:

```sh
python3 scripts/migrate_registry_pr.py "$base" "$approved_head"
```

The command reads committed old inputs, not conflict markers. It prints changed
new-layout paths (including exact provenance); it never stages or removes legacy
files. After success, `git rm` each conflicted old subsystem JSON, the old shared
diagnostic JSON when present, and their old provenance records. Stage the printed
paths, including `codes.rs` when changed. Refuse any remaining unmerged paths
outside the existing mechanical resolver's scope, then run cheap and the
approved-patch identity check before pushing and re-approving.

Only registry data is translated. Conflicting edits, unrepresentable layouts,
unkeyed object removal/replacement, changed Rust constructors, Rust/JSON
disagreement and header changes fail before
writes. Main's entry order is retained; PR additions append in their original
order. Re-running the same base/head changes no bytes. The committed fixture
contains exact old registry diffs from approved PRs #372 and #409 with their SHAs;
the offline tests merge each onto a migrated tree with concurrent additions.
