# Norn Agent Skills

Reusable, domain-agnostic skills for coding agents building and operating applications on **Norn** alongside the `norn` CLI and MCP interfaces.

## Available Skills

| Skill | Use when |
| --- | --- |
| [`norn-app-author`](./norn-app-author/SKILL.md) | Modeling a business domain in typed TypeScript (`@norn/authoring`), checking invariants with `norn check` and `Norn.example`, and installing definitions straight onto a Norn Cell — with zero custom CRUD server. |
| [`norn-ui`](./norn-ui/SKILL.md) | Building a Norn Studio or tenant web UI over a Norn Cell using Base UI, Vercel UI SDK, Geist, Tailwind v4 `data-skin` tokens (Open v6 + Polar), honest action/refusal handling, and server-only redaction. |
| [`norn-performers`](./norn-performers/SKILL.md) | Writing thin, stateless external world-boundary performers behind `Norn.fn` / `Norn.perform`, using Core-07 / D03 uncertainty recovery (`outcome-unknown` -> reconciliation) and authenticated Function receipts. |
| [`norn-cell-ops`](./norn-cell-ops/SKILL.md) | Provisioning, updating (`plan` / `apply`), subscribing (`norn.live.v1`), and operating live Rivet Actor (`norn-cell-rivet`), Durable Object (`norn-cell-do`), or local `norn dev` cells via CLI and HTTP MCP. |

## Core Rule Across All Skills

**The Norn Cell IS your backend.** Do not build a parallel Express, Hono, FastAPI, or bespoke SQLite/Postgres CRUD API alongside Norn definitions. Put domain state, lifecycles, laws, markings, and authorization inside the cell; keep only thin, stateless world-boundary performers outside.
