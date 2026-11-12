# Norn project

Edit src/; never write IR.
Use exact decimals with Norn.decimal; never Schema.Number for money.
Keep identities, scope and writes explicit.
Use Norn.type and Norn.action with the current authoring syntax.

## Loop

The norn command is the installed @norn/cli; run it as bunx norn in this project.
Run bunx norn fmt to format sources; add --check to check without writing.
Run bunx norn check src --org <orgId> --env <environmentId> to compile and admit; the scope is in norn.json.
Run bunx norn dev to serve the project's cell on loopback; every save is checked and installed or refused.
Run bunx norn explain action|law|concept <id> to see what a declaration reads, writes and must preserve.
Add --json to dev or explain for machine-readable output; check always prints JSON.
Run bunx norn --version to see the CLI version and the commit it was built from.
Plan, apply, deploy and an MCP endpoint are not available yet.

## Agents

Call actions over HTTP at the url in .norn/dev.json with the bearer token in .norn/dev.token.
The .norn/dev.json format and the dev --json events are experimental.
Use the same checked actions as a human.
Do not edit .norn/ or generated IR.
A refused edit leaves the last good revision serving.
Fix the reported source location and check again.
Never bypass admission or weaken a law.
Keep changes inside src/ when authoring the application.
Unknown evidence is not proof.
