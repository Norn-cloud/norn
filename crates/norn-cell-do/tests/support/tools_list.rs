//! MCP-TOOLS-SNAPSHOT-1 (#1762): the exact bytes of a `tools/list` answer,
//! kept under `tests/fixtures/mcp-tools-list`. Agents rely on the tool names,
//! schemas and annotations; a change to where the catalog comes from must not
//! change them. `NORN_BLESS=1` rewrites the fixture.

/// Check `body` against the fixture `name`, byte for byte.
pub(crate) fn snapshot(name: &str, body: &str) {
    let file = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-tools-list")
        .join(name);
    let text = format!("{body}\n");
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        let blessed = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&file, &text));
        assert!(blessed.is_ok(), "{name}: not blessed: {blessed:?}");
    }
    let kept = std::fs::read_to_string(&file);
    assert!(
        kept.as_deref().is_ok_and(|kept| kept == text),
        "{name}: tools/list differs from the snapshot byte for byte (or it is missing); review it and rerun with NORN_BLESS=1"
    );
}
