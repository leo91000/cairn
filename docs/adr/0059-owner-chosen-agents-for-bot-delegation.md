# Owner-chosen agents for bot delegation

Date: 2026-10-24.

This supersedes the agent restrictions of
[ADR-0038](0038-luau-installation-plugins-and-external-bot-delegation.md): a
dedicated agent with at most the selected project and no installation access for
listed or approved people, and a read-only agent without MCP or plugin access for
anyone in a channel.

## Context

Those rules suited a public support channel, but they also applied to a private
server shared by two people who trust each other. There they forced a second agent
whose access copied the owner's, and they kept the main agent from answering at
all. Owners already choose who may reach a bot: listed people, people approved as
they ask, or anyone in the channel.

## Decision

Any agent may answer a delegation, whoever it is delegated to. The installation no
longer refuses an agent's projects, installation access, sandbox, MCP connections or
plugin grants when a delegation is saved, reapproved or used. The web and Android
setup assistants instead warn the owner about what the chosen agent lets these
people do: manage the installation, delegate work to other coding agents and models, and
open several or every project, and for
anyone in the channel, also write files, run commands and use MCP connections or
plugins.

Unchanged: the agent must be able to open the delegation's project. A delegation
stays bound to the owner who set it up and to the agent policy they approved, so
changing the agent's access suspends the bot until it is reapproved. People
outside the list are ignored or become requests, the hourly limits still apply,
and only listed people or a conversation's starter can answer for it or stop it.

## Consequences

An owner who lets anyone in a public channel reach a broad agent exposes that
agent's whole access to them. The warning names this, and suggests a read-only
agent with one project for public channels. Plugin grants no longer suspend a bot
anyone can reach, as no rule depends on them.
