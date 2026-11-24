# Norn Agent Skills

Reusable, domain-agnostic skills for coding agents building and operating applications on **Norn** alongside the `norn` CLI and `norn mcp` server.

## Available Skills

| Skill | Use when |
| --- | --- |
| [`norn-app-author`](./norn-app-author/SKILL.md) | Modeling a business domain in typed TypeScript (`@norn/authoring`), checking invariants with `norn check` and `Norn.example`, and installing straight onto a Norn Cell — with zero custom CRUD server. |
| [`norn-ui`](./norn-ui/SKILL.md) | Building an Open-Canvas tenant UI over a Norn Cell using Base UI, Vercel UI SDK, Geist, Tailwind v4 `data-skin` tokens, `boneyard-js`, and server-only PII redaction. |
| [`norn-performers`](./norn-performers/SKILL.md) | Writing thin, stateless external world-boundary performers (payment rails, messaging bots, CP-SAT/ML solvers, legacy read-only adapters) behind `Norn.fn` and the cell's atomic outbox. |
| [`norn-cell-ops`](./norn-cell-ops/SKILL.md) | Provisioning, updating (`plan` / `apply`), subscribing (`norn.live.v1`), and debugging live Rivet Actor (`norn-cell-rivet`) or Durable Object (`norn-cell-do`) cells via CLI and MCP. |

## Core Rule Across All Skills

**The Norn Cell IS your backend.** Do not build a parallel Express, Hono, FastAPI, or bespoke SQLite/Postgres CRUD API alongside Norn definitions. Put domain state, lifecycles, laws, PII markings, and authorization inside the cell; keep only thin, stateless world-boundary performers outside.
