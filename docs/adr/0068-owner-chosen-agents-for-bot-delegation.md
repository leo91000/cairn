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
people do: manage the installation and open several or every project, and for
anyone in the channel, also write files, run commands and use MCP connections or
plugins.

Unchanged: the agent must be able to open the delegation's project. A delegation
stays bound to the owner who set it up and to the agent policy they approved, so
changing the agent's access suspends the bot until it is reapproved. People
outside the list are ignored or become requests, the hourly limits still apply,
and only listed people or a conversation's starter can answer for it or stop it.

For a native agent, selecting or reapproving it records the authenticated caller's
existing native authority in the same configuration transaction. Admitted plugin
messages use that explicit delegation; they do not claim to be a Cairn person.
Native work retains the connection revision, external author, owner binding and
node ceiling. Admission and lease renewal recheck the connection, its approved
agent policy, allowed people and fresh owner access. Disabling or replacing the
delegation revokes its work and descendants. Questions and resumed turns retain
the same provenance.

This extends ADR-0050 only for known, explicitly approved plugin delegations.
Unknown callers, historical connections without that saved grant, and MCP
management requests without native authority cannot mint a native delegation.
Saving an existing bot through the authenticated owner configuration records its
grant without another approval step or a change to the selected agent.

## Consequences

An owner who lets anyone in a public channel reach a broad agent exposes that
agent's whole access to them. The warning names this, and suggests a read-only
agent with one project for public channels. Plugin grants no longer suspend a bot
anyone can reach, as no rule depends on them.
