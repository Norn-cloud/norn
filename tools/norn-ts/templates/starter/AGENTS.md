# Norn project

Edit src/; never write IR.
Use exact decimals with Norn.decimal; never Schema.Number for money.
Keep identities, scope and writes explicit.
Use Norn.type and Norn.action with the current authoring syntax.

## Loop

Run norn fmt to format sources.
Run norn fmt --check to check formatting without writing.
Run norn check to compile and admit the project.
Run norn dev to serve the local project and watch edits.
Run norn plan to inspect a proposed revision.
Run norn apply to install the reviewed plan.
Run norn explain <id> to inspect diagnostics and evidence.
The dev, project check, plan, apply and explain commands arrive in later units.
Until then use the repository CLI with an explicit source directory and scope.

## Agents

The dev MCP endpoint is http://127.0.0.1:4174/mcp when dev is available.
Read .norn/dev.json for the actual running session and token file.
Use the same checked actions as a human.
Do not edit .norn/ or generated IR.
A refused edit leaves the last good revision serving.
Fix the reported source location and check again.
Never bypass admission or weaken a law.
Keep changes inside src/ when authoring the application.
Unknown evidence is not proof.
