# 0034 — Conversation writers

Version 33 upgrades to 34 automatically in `Store::open`, before the SQL actors start
([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Autorité des
règles » and « Sans migration »).

One immediate transaction creates six empty tables and advances `user_version`:

- `conversation_writers`: who created each conversation (a person, or no one), its
  agent, the current person's plugin-instruction baseline, and the first other writer
  to reach it, including the person's account or the reaching rule's identifier.
- `conversation_entry_writers`: who wrote each message as it is stored now, with a
  digest of its content: messages, steers, answers and edits.
- `conversation_turn_entries`: the message that started each turn and the first launch
  grant bound to it, with the exact instruction source frozen for that turn.
- `agent_instruction_writers`: who wrote each version of an agent's instructions.
- `native_instruction_writers`: who wrote installation and agent native instruction defaults.
- `skill_writers`: who wrote each skill revision through Cairn's skill tools.

Nothing written before has a recorded writer: nothing is converted, and earlier
conversations, instructions and skills never let agents write rules. A failure leaves
the database at 33. A successful upgrade has no downgrade path. Reopening an upgraded
database changes nothing.
