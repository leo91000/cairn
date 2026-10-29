> Statut : l’étape `people` (la délégation externe) est retirée par [ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md), et une instance ne compte comme accordée à un agent de gestion, pour réutiliser une étape partagée, que s’il en a l’accès complet. Le MCP public peut accorder des droits, `agentIds` compris à la dernière étape, qui donne l’accès complet ; `cairn_installation` ne le peut pas.

# Guided plugin setup and shared provider apps

Date: 2026-10-22.

## Context

Every plugin instance was configured in one generic form: name, callback URL,
sign-in, the package's guidance, every setting, the agents note and the activity
switch, in manifest order. The order did not follow what each step needs. For
Gmail, the sign-in button came before the OAuth client it requires; the Google
Cloud instructions were one paragraph of package text; eight permissions had the
same weight; and the OAuth client was asked again for every mailbox although it
belongs to the owner's Google Cloud project. Owners could not tell whether Gmail
needed only a Google sign-in or a Google Cloud setup first.

## Decision

A manifest may declare `[setup]`: the effort connecting takes (`signin`, `token`
or `app`), a summary, the setting that names a new instance (`nameFrom`) and
ordered steps. A step is `settings` (declared fields, optional checklist of tasks
on the provider's site, presets of its switches, warning), `credential`, `people`
(the external delegation) or `agents`. Declared steps cover the whole instance:
the credential, every visible setting, the delegation and agent access, with
agents last. Without steps, the installation derives them: settings then sign-in
for OAuth, the credential first otherwise, then people and agents. Boolean
settings may declare a `risk`.

The installation resolves the setup (`setup` beside each catalogue manifest and in
the management `setup` action). Web and Android render it as the same assistant,
natively. Clients consume this resolved setup and require a current installation.
A shared corpus (`tests/fixtures/plugin-setup.json`) keeps Rust, web and Android
equal.

A step marked `shared` holds values that serve every instance, such as an OAuth
client and its secret. A new instance names a saved instance of the same package
with `sharedFrom` on its form, sign-in and save; the installation copies those
values, secrets included, without sending secrets to the client. Each instance
keeps its own copy, as before: the OAuth credential record already stores the
client used for refresh, so connection and vault formats stay unchanged. An installation
management agent may reuse only an instance it is granted, like any saved
credential.

The assistant's last step may grant agents to the new instance, through the save's
existing `agentIds`. As for the last step of adding a node in
[agent-centric permissions](0051-agent-centric-permissions.md), creation only
grants and never removes. A saved instance lists its agents read-only; grants
change on agent pages. Public MCP and the installation tool still refuse
`agentIds`.

## Consequences