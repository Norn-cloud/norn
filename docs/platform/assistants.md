# Use Norn from Claude or ChatGPT

Norn works as a connector in the assistant you already use. You ask in the chat; the assistant
calls Norn's tools as you, and Norn answers with its own views inside the conversation: an answer
with its chart, the records behind a number, a proposed dashboard, an action's receipt.

The assistant gets no special access. Every call runs as the person who connected it, in the
workspace they chose, and Norn's rules decide it exactly as they do in Studio. Reads return only
what you may read. Actions are accepted with a receipt or refused with the reason. There is no
SQL or table access.

## Before you start

- A Norn Studio account on the alpha, in a workspace that has a data environment. The alpha is
  invite-only; ask your Norn contact for an invitation.
- The connector URL: `https://<alpha Studio origin>/mcp`. Your invitation names the origin.

## Claude

1. In Claude, open **Customize → Connectors** and choose **Add custom connector**. On Team and
   Enterprise plans an owner adds it once under **Organization settings → Connectors**; members
   then connect it for themselves.
2. Name it `Norn` and paste the connector URL. Leave the advanced OAuth fields empty.
3. Choose **Connect**. A Norn page opens: sign in with your Studio email and password. If you are
   already signed in to Studio in this browser, Norn skips this step.
4. Norn shows which application is asking and the workspace it will act in. Choose **Allow**.
5. In a chat, turn Norn on from the tools menu and ask, for example: "Using Norn, why is last
   week's gold yield an outlier?"

Claude renders Norn's views inline on every plan. On Team and Enterprise plans an owner can turn
interactive views off; Norn's answers then arrive as text.

## ChatGPT

1. In ChatGPT, open **chatgpt.com/plugins**, choose **+**, then **Add custom MCP server**.
   Custom servers need developer mode, which depends on your plan and your workspace settings.
2. Paste the connector URL and choose OAuth sign-in.
3. Sign in to Norn and choose **Allow**, as for Claude.
4. In a chat, pick Norn from the tools and ask your question.

## What you can ask

- "What does this workspace hold?" The assistant reads the catalog: object types, their
  properties, the admitted questions and the actions you may call.
- "Show weekly gold yield" or "Which records are behind week 41?" You get an answer card with
  the exact values and the records and receipts behind each one.
- "Propose a dashboard of yield by week." Norn's checker reads the proposal and says in plain words
  what it means. Nothing is saved until a person installs it in Studio by signing that restatement.
- "Record a melt of 12 g in, 10 g out." The action is decided by the workspace's laws and comes
  back as a receipt or as a refusal with the reason. A retried call returns the original receipt.

## Disconnect

Remove the connector in your assistant's settings. Leaving the workspace in Studio also stops the
connector at once: Norn checks your membership on every call.

## When something goes wrong

- **"Sign in to Norn to use this connector."** The grant expired or was revoked. Reconnect it from
  the assistant's connector settings.
- **"This workspace has no data environment yet."** Your workspace has no cell yet; ask your Norn
  contact.
- **The sign-in page says the link expired.** Start connecting again from the assistant. Sign-in
  links last a few minutes.
- **No views, only text.** The host has interactive views turned off, or the deployment has no
  widget build. The answers are the same.

## For operators

- Turn the connector on per stage with `STUDIO_MCP=on`. Apply migration
  `apps/studio/worker/migrations/0007_oauth_provider.sql` before that deploy, and bind the
  stage's Norn host (`NORN_HOST`).
- Studio is the OAuth 2.1 authorization server: PKCE, resource-bound access tokens (RFC 8707),
  client registration by metadata document (only `claude.ai`, `claude.com` and `chatgpt.com`
  are fetched) or dynamic registration. Either way, only the assistants' own callback URLs can
  receive a code.
- Discovery is at `/.well-known/oauth-protected-resource/mcp` and
  `/.well-known/oauth-authorization-server`. An unauthenticated call to `/mcp` answers 401 with
  `WWW-Authenticate: Bearer resource_metadata="…"`.
- The cell's `POST /v1/mcp` projects the tools from what the cell declares, and every tool call
  re-enters the cell's own routes as the caller.
