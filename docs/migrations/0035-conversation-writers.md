# 0035 — Conversation writers (SQLite schema 34)

Version 33 upgrades to 34 automatically in `Store::open`, before the SQL actors start
([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Autorité des
règles » and « Sans migration »).

One immediate transaction creates seven empty tables and advances `user_version`:

- `conversation_writers`: who created each conversation (a person, or no one), its
  agent, the current person's plugin-instruction baseline, and the first other writer
  to reach it, including the person's account or the reaching rule's identifier, and a
  permanent latch when a turn follows unattested instructions.
- `conversation_entry_writers`: who wrote each message as it is stored now, with a
  digest of its content: messages, steers, answers and edits.
- `conversation_skill_sources`: every skill revision followed by the conversation,
  including dynamic reads, retained after a later turn stops selecting it, and the
  writer-history limit at the time it was followed.
- `conversation_turn_entries`: the message that started each turn and the first launch
  grant bound to it, with every instruction source frozen for that turn; deleting a
  run or message does not remove this conversation history.
- `agent_instruction_writers`: who wrote each version of an agent's instructions,
  retained after agent deletion so reusing an identifier cannot reset authorship.
- `native_instruction_writers`: who wrote complete installation and agent native
  documents for every provider, with inheritance masks.
- `skill_writers`: who wrote each skill revision through Cairn's skill tools, bound
  to its canonical directory as well as its logical scope.

Nothing written before has a recorded writer: nothing is converted, and earlier
conversations, instructions and skills never let agents write rules. A failure leaves
the database at 33. A successful upgrade has no downgrade path. Reopening an upgraded
database changes nothing.
