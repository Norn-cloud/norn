# sidekick-bridge

Studio's Sidekick agent over the Agent Client Protocol (ACP). `relay.ts` is one
Bun process: each browser WebSocket gets its own Claude Code ACP adapter
(`@agentclientprotocol/claude-agent-acp`, over stdio), and the relay passes ACP
messages both ways.

The agent's only tools are the Norn MCP server. Built-in tools are off, no
settings, skills, plugins or other MCP servers load, and file system and
terminal requests are refused. The browser proves who it is with a short-lived
Sidekick grant minted by Studio's Worker (`POST /api/sidekick/grant`); the relay
verifies it with `jose` against Studio's signing keys and adds it to each call to
Studio's `/mcp`, so the cell's policy decides every call as that person. The
grant never reaches the agent.

It runs on one subscription login and is a demo bridge; the header of
`relay.ts` lists the full trust rules.

## Run

```sh
bun install --frozen-lockfile
bun test          # relay.test.ts
bun relay.ts      # or: bun run start
```

Configuration is read from the environment (`configFromEnv` in `relay.ts`).
Required: `SIDEKICK_STUDIO_ORIGIN`, `SIDEKICK_LOGIN_HEADER`, `SIDEKICK_ADAPTER`
(the adapter command), `SIDEKICK_SCRATCH`, `SIDEKICK_ALLOWED_LOGINS` (comma-separated;
without it the relay still starts but answers every request with 403), and
`SIDEKICK_ALLOWED_ORIGINS` (comma-separated Studio origins; without it every `/ws`
upgrade is refused with 403).
Optional: `SIDEKICK_LISTEN_HOST` (default `127.0.0.1`),
`SIDEKICK_LISTEN_PORT` (default `17950`), `SIDEKICK_MCP_PORT` (default `17951`),
`SIDEKICK_MAX_CONNECTIONS` (default `3`), `SIDEKICK_MODEL`.

`sidekick-bridge.service` is a `systemd --user` unit for running the relay on an
agent host; its header shows an example environment file.
