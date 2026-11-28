# 0037 — Hook rule conversations (SQLite schema 35)

This guide describes a historical migration in the schema-37 manager at
`a0109b792fa43d2e076073f34901d57eae2e3615`. See the current
[upgrade procedure](../PRODUCTION-CAIRN.md#data-preserving-upgrades) for schema 38.

Version 34 upgrades to 35 automatically in `Store::open`, before the SQL actors start
([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Clé de conversation »,
« Hooks de Cairn » and « Budget du moteur de règles »).

One immediate transaction creates ten empty tables and advances `user_version`:

- `hook_rule_conversations`: each conversation a rule started, with its creator, its
  key (unique per creator, released when the conversation is deleted or detached by a
  merge), the external origin and author, and where Cairn stands in its run to emit
  Cairn hooks: the last event read and the current turn's origin, chain depth,
  reported questions and progress.
- `hook_rule_conversation_values`: the values rules remember on a conversation, at
  most 64 KiB each; deleted with the conversation.
- `hook_rule_turn_origins`: who wrote each entry of a conversation in a rule chain (a person, a
  rule with its chain depth, an agent, `cairn_installation` or unknown), so that the
  hooks of the turn it starts report it.
- `hook_rule_chain_depths`: the depth recorded synchronously when an input is
  queued, including unkeyed conversations reached by installation access or
  delegation. It prevents agents from lowering a chain before its events have
  been observed; a person's input starts a new chain.
- `hook_rule_task_depths` and `hook_rule_run_depths`: the causal depth frozen with
  agent-created or edited scheduled work and with a mission run's admission. A
  mission cannot lose the chain of the conversation that started it, even if
  that conversation subsequently accepts another person's input or is deleted.
  Every chat turn in a chain records its active depth at launch and extends it
  on delivery or rule cancellation, independently of queued inputs and the
  asynchronous hook observer. A queued human input resets it when its own turn
  launches, including when a mission is continued as a conversation.
- `hook_rule_question_order`: a creation receipt with the run's last event position,
  recorded atomically with each questionnaire on a keyed conversation. Question
  hooks retain their turn's order even if timestamps tie or move backwards, and
  remain reportable after an answer or withdrawal.
- `hook_rule_reported_questions`: once-only notifications retained until their
  question is deleted, processed in bounded batches without dropping
  deduplication at a cache limit or after restart.
- `hook_rule_author_triggerings`: the triggerings each author caused, for quota
  conditions, kept a week.
- `hook_rule_conversation_counts`: the conversations each rule started or steered in
  the last hour, for its conversation cap; lifting a budget clears them.

No conversation is bound to a key before; nothing is converted. Conversations started
earlier, including those of plugin delegation, have no key and emit no Cairn hook. A
failure leaves the database at 34. A successful upgrade has no downgrade path.
Reopening an upgraded database changes nothing.
