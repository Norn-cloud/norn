@AGENTS.md

## Norn authoring

Before writing or reviewing Norn definitions or engine features, every agent must read:

- [The authoring reference](docs/authoring/README.md), including CURRENT versus TARGET syntax.
- The relevant `docs/spec/core/01–08` file selected through the [core spec index](docs/spec/core/README.md), and its named acceptance scenarios.
- [The 2026-11-01 decision record](docs/architecture/decisions/2026-11-01.md), which governs older designs.

Use the [norn-authoring skill](.claude/skills/norn-authoring/SKILL.md) for definitions, packs, laws, mappings and authoring SDK work. Implement specified invariants, refusals and scenarios; report ambiguous contracts rather than inventing domain kernel behavior.
