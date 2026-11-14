# 0035 — Hook rule conversations (SQLite schema 34)

Version 33 upgrades to 34 automatically in `Store::open`, before the SQL actors start
([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Clé de conversation »,
« Hooks de Cairn » and « Budget du moteur de règles »).

One immediate transaction creates five empty tables and advances `user_version`:

- `hook_rule_conversations`: each conversation a rule started, with its creator, its
  key (unique per creator, released when the conversation is deleted or detached by a
  merge), the external origin and author, and where Cairn stands in its run to emit
  Cairn hooks: the last event read and the current turn's origin, chain depth,
  reported questions and progress.
- `hook_rule_conversation_values`: the values rules remember on a conversation, at
  most 64 KiB each; deleted with the conversation.
- `hook_rule_turn_origins`: who wrote each entry of a keyed conversation (a person, a
  rule with its chain depth, an agent, `cairn_installation` or unknown), so that the
  hooks of the turn it starts report it.
- `hook_rule_author_triggerings`: the triggerings each author caused, for quota
  conditions, kept a week.
- `hook_rule_conversation_counts`: the conversations each rule started or steered in
  the last hour, for its conversation cap; lifting a budget clears them.

No conversation is bound to a key before; nothing is converted. Conversations started
earlier, including those of plugin delegation, have no key and emit no Cairn hook. A
failure leaves the database at 33. A successful upgrade has no downgrade path.
Reopening an upgraded database changes nothing.
