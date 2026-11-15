# Hook rules

A hook rule applies a tree of steps to each event of one hook
([ADR-0075](adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Règles de hook »).
It answers « when X happens, do Y » without code: a plugin declares hooks, tools and
resources; a person writes the rule. This guide describes the rule model, who may do
what with it and how the web app, Android app, public MCP and `cairn_installation`
reach it. The rule engine runs the stored versions.

## A rule

| Part | Meaning |
| --- | --- |
| `trigger` | A plugin hook (`{type: "plugin", instanceId, hook}`) or a Cairn hook for conversations bound to the creator's keys (`{type: "cairn", hook}`: `replyReady`, `questionAsked`, `progress`, `ended`). |
| `executionAgentId` | The agent whose rights frame the plugin hook the rule listens to and the tools it calls. It must hold the hook; its creator must have access to it. Designating it does not make it work and needs no native authority. |
| `steps` | The tree: `if` (condition, `then`, `otherwise`), `forEach` (a list, the item's name, a body) and `action` blocks. Every block has an `id`, unique in the rule; an action's result is `steps.<id>` for the actions that run after it. |
| `limits` | Hourly caps on triggerings and on conversations started or steered (defaults 60 and 10, at most 600 and 100). |

There is no filter outside the tree: a bot open to anyone is a rule that does not
test the author.

### Values

Every value of an action or condition is one of:

- `{type: "literal", value}`: a fixed JSON value (at most 16 KiB);
- `{type: "template", text}`: text where each `{{expression}}` inserts the result of a
  CEL expression, such as `discord:{{threadId}}`. An expression ends at the first `}}`;
  write `{{"{{"}}` for literal braces;
- `{type: "formula", expression}`: a [CEL](https://cel.dev) expression.

The agent an action starts, and the tool and instance it calls, are chosen in the rule,
never computed by a formula.

Formulas read `payload` (the event), each top-level payload field by its own name
(`threadId`), `steps`, `values` (the rule's remembered values), `event` (`id`, `hook`,
`instanceId`, `receivedAt`), `now` and the item of each enclosing loop. The payload of
a [Cairn hook](#cairn-hooks) carries the values remembered on its conversation in
`payload.values`. They call the
CEL standard library, the lists extension, the string functions of the strings
extension (`charAt`, `indexOf`, `lastIndexOf`, `lowerAscii`, `upperAscii`, `replace`,
`split`, `join`, `substring`, `trim`), which Cairn implements with size checks, and these
Cairn functions:
`truncate(text, length)`, `chunks(text, size)`, `formatDate(time, format, timezone?)`,
`lower(text)`, `upper(text)`, `toJson(value)` and `fromJson(text)`. A formula is pure
and its cost is bounded. Saving checks it: at most 2,000 characters and 300 nodes, no
more than two nested `all`/`exists`/`exists_one`/`map`/`filter`, no namespaced
function (such as `lists.range`), no message construction and no optional syntax. The
rule engine also caps the size of every intermediate value and the iterations of every
macro while it evaluates one.

### Conditions

`all` and `any` groups nest up to six levels, with `not`. `compare` applies an operator
to a payload field or formula (`equals`, `notEquals`, `contains`, `startsWith`,
`endsWith`, `matches` a whole regular expression, `in` a list, `present`, `absent`,
`greaterThan`, `lessThan`). `formula` is a boolean CEL expression. Cairn predicates:
`authorIn` (the author is one of the chosen people, as plugin resources),
`authorStartedConversation` (the author recorded when a rule started the conversation
of a key is this one), `conversationExists` (a conversation, not deleted, is bound to
a key of the rule's creator) and `authorQuota` (the author triggered the rule fewer
than `limit` times in `windowMinutes`). An author is text or a number of at most 200
characters; a missing author makes these author predicates false. A key is text of 1
to 200 characters, otherwise the triggering fails. A triggering counts for the
authors its quota conditions examined once it runs an action, once per author; at
most 20 authors per triggering.

### Actions

`callTool` calls a plugin tool through the execution agent's grants, never a Cairn
tool; its literal inputs are checked against the tool's input schema. `startAgent`
starts an agent the creator may use with a prompt, in a project or not, optionally
bound to a key and recording the external `author` it starts for; it fails when the
key is already bound, and the tree chooses to steer instead. `steerConversation`,
`cancelConversation` and `answerQuestion` reach a conversation only by its key (see
[conversation keys](#conversation-keys)). `storeValue` remembers a value with the
rule, or with the conversation of a key. `wait` waits from one second to seven days,
then resumes the next steps; a loop cannot wait.

A rule holds at most 200 blocks nested eight levels deep. One triggering runs at most
500 blocks and iterations, actions excluded, and 50 tool calls.

The catalog also publishes fixed data limits: a triggering records at most 1 MiB
of event and step data (with room reserved for failure details), its persisted
control state is at most 512 KiB, and a rule's remembered values total at most
1 MiB. Combined tool input is at most 256 KiB. Exceeding a limit stops the
triggering; an action whose result could not be recorded is never retried.
Results live in the step records and are loaded by node identifier when execution
resumes, rather than copied into every persisted cursor.

## The engine

Each queued plugin hook event is copied into a private admission for every eligible
listening rule when capacity allows. The shared instance queue is acknowledged in the same transaction,
so other rules keep receiving later events. An event no rule may receive is dropped.
Once its execution agent's hook grant covers every resource the payload designates,
the admission becomes a triggering, pinned to the rule version saved at fanout.
Admissions wait at most ten minutes from fanout, including time waiting for a
resolver slot. Expiry counts as `notAdmitted`, once, without retaining the payload;
time spent in the transport queue before fanout does not consume this window.
Resolution is limited to one operation per rule and eight in total, with a ten-second
read deadline and backoff for the affected rule when resolution fails temporarily.
The same event is never triggered twice for one rule.

The engine runs at most one action per rule at a time, returning its executor slot
after each action. Least recently served rules receive the next free slots; waits
and retries release their slots until due. The engine walks each tree in order.
Before every action it checks the rule again: a
disabled, paused, rights-lost or ended rule stops its triggerings, while an exhausted
budget lets those already counted finish. A rule blocked while some of its triggerings
wait stops them. A member's triggering acts only once their
membership was verified within the last minute, and again after each wait; while the
Beacon cannot confirm it, the triggering waits, and fails after seven days. Member
triggerings use at most four of the eight executors, leaving capacity for owner
rules during a Beacon outage.

Variables:

| Variable | Value |
| --- | --- |
| `payload` and each top-level payload field | the event |
| `steps.<id>` | the result of an earlier action: the tool's answer, the stored value, or `{resumesAt}` for a wait; null for an action of the tree that did not run |
| `values.<name>` | the rule's remembered values |
| `event` | `id`, `hook`, `instanceId`, `receivedAt` |
| `now` | when the triggering started, as a CEL timestamp |
| a loop's item name | the current item |

A comparison reads a missing payload field as null: `absent` holds, `equals` does not.
A loop iterates at most 1,000 items. Formula evaluation stops when a value grows above
256 KiB, when its macros iterate more than 20,000 items in total, or when it produces
more than 16 MiB of values.

Actions:

- `callTool` evaluates its inputs, checks them against the tool's input schema, then
  calls the tool through the execution agent's grants, restricted to the instance the
  rule names. The triggering records the action as in flight before calling: if Cairn
  stops before the answer, the action is never run again and the triggering fails, as
  the action may have taken effect. A tool that answers exactly `{retryAfter: seconds}`
  (optionally with `error`) tells Cairn the service did nothing: only that action runs
  again, after the larger of that delay and an exponential backoff, for at most five
  attempts in all. A requested delay beyond seven days fails the triggering. Retries
  share the rule's waiting limit, and membership is checked again before resuming.
  Replies with `ok: false` end the triggering as failures. They retry only when the
  plugin explicitly sets `retryable: true`; a status such as 429 or 503 alone never
  confirms a mutation was not performed. Numeric and HTTP `Retry-After` values are
  supported. Transient refusals detected by Cairn before invocation also retry.
  Any other error ends the triggering with its message.
- `storeValue` with the rule scope keeps up to 64 KiB.
- `wait` resumes the next steps later; a rule keeps at most 20 triggerings waiting,
  including retries. Membership verification waits retain their seven-day deadline.
  A rule can hold at most 1,000 active triggerings and unresolved admissions combined,
  and at most 16 MiB of event data across them; later events are refused only
  for that rule and counted as `notAdmitted` in its history, without retaining their
  payloads or blocking the shared instance queue.
- Conversation actions (`startAgent`, `steerConversation`, `cancelConversation`,
  `answerQuestion` and `storeValue` with the conversation scope) take effect in the
  transaction that records them: an interruption leaves either both or neither, so
  they are never ambiguous and never replayed. A refusal (a bound key, a lost access,
  a person's pause, the chain depth, native authority) is recorded as the failed
  action and ends the triggering; nothing it would have changed is kept. Their result
  is `{conversationId, messageId, key}` for a start, `{conversationId, messageId}` for
  a steer, `{conversationId, questionId, messageId}` for an answer,
  `{conversationId, runId?, cancelled}` for a cancellation and the value for
  `storeValue`.

### Budget

A triggering counts against the hourly cap when it runs its first action. Beyond the
cap, that triggering fails, the rule becomes `budgetExhausted` and its creator receives
an ordinary push notification; the triggerings already counted finish. Push delivery
respects device preferences and the usual delivery window. The lasting signal is the
`budgetExhausted` block, exposed by list/get until the creator or owner lifts it. A triggering that ends without
any action is not kept, including failures and stops before the first action: the
history only counts them by hour. Each triggering runs at
most 500 control steps and 50 tool calls (retries included). Each `if` or `forEach`
block counts once, and each loop iteration also counts once, including the first;
actions do not count as control steps. A loop with only actions can therefore run
at most 499 iterations within that limit.
Starting or steering a conversation also counts against the hourly conversation cap
(default 10, at most 100), even from a progress triggering; beyond it, the action
fails and the rule becomes `budgetExhausted` like the triggering cap. Lifting the
budget gives both caps a fresh window. Triggerings of the `progress` hook, already
paced by Cairn, do not count against the triggering cap.

A chain is what Cairn can follow: a rule's conversation action causes a Cairn hook,
whose triggering acts on a conversation, which causes another hook, and so on. A plugin
event starts a chain at depth 0 and only a person's input starts a new one; each start,
steer, answer or cancellation by a rule makes the hooks of the turn it causes one link
deeper. Any other input keeps the chain it stands in: a turn without new input (a
wakeup, the end of a wait or of a delegated task), an entry of the agent through
`cairn_installation` (at least as deep as the run that wrote it) and an entry Cairn did
not attribute keep the conversation's depth. Cairn follows at most **8** links (`maxChainDepth` in the
catalog): a conversation action that would make a ninth fails with an error naming
the limit. Only Cairn records the depth, beside the event and outside its payload, so
a plugin's payload cannot supply or lower it. A tool call that makes the service emit
a plugin event again is not a chain Cairn can follow: the hourly caps bound it.

The depth also follows mission runs started through `cairn_installation` and
scheduled work created or edited by an agent. It is saved with that work and its
run, independently of later input in the conversation that caused it. Manual
context compaction is maintenance: it emits neither a new reply nor an end-of-turn
hook, including when its events are observed after the next real turn starts.

### History

Every member sees the state of every triggering of every rule
(`list_hook_rule_triggerings`, newest first, 50 per page, with the number of
triggerings without effect in the last 24 hours). The event payload, each action's
input, result and error (`get_hook_rule_triggering`) are visible to the creator, the
owner and members with access to the execution agent of the version that triggering
ran with. Cairn keeps the 200 newest triggerings of each rule, and every one still
running, waiting or retrying. Hourly accounting is retained independently of this
history, and lifting an exhausted budget gives subsequent triggerings a fresh window.

## Conversation keys

A key is text of 1 to 200 characters, usually a template such as `discord:{{threadId}}`.
`startAgent` binds the conversation it starts to a key; keys belong to the rule's
creator, so two creators using the same key name two conversations, and a rule only
reaches its creator's keys. A key is free again once its conversation is deleted
(moved to the trash or purged); a finished conversation keeps its key and can be
steered again. Merging agents detaches the keys of the conversations the merge moves,
with the values rules remembered on them: no rule reaches them anymore.

Every action on the conversation of a key needs its creator's current access to that
conversation's agent. Steering queues a message that Cairn delivers after the current
turn, as for a person who writes again, also after a failed or stopped turn. A paused
conversation cannot be steered or answered until a person resumes it.
`cancelConversation` stops the current turn like Stop in Cairn, which pauses the
conversation. Only a person can resume it, including after a rule's cancellation.
With nothing working, `cancelConversation` only cancels a scheduled
wakeup.
`answerQuestion` answers the oldest pending question of the conversation's current
run: a text answers every question of it, an object answers each question by its id
with a text or `{selected, comment}`.

Values remembered on a conversation (`storeValue` with the conversation scope) are
shared by every rule that reaches it, at most 64 KiB each and 256 KiB per conversation,
and are deleted with it. Cairn hooks carry them in `payload.values`.

A conversation a rule starts has the rule's creator as author, without making it
private ([ADR-0029](adr/0029-sharing-whole-installations.md)), and records its external
origin: the rule, the hook, the plugin instance, the event and the `author` the action
gave. Agents never write rules in a conversation a rule started or reached.

### Native authority

Each conversation action resolves an agent or a conversation: when that agent may only
run natively, or the conversation runs natively, the action needs both the native
authority saved with the rule's version and its creator's current native authority
([ADR-0050](adr/0050-trusted-native-execution.md)); without them it fails. The
conversation then carries that saved authority narrowed to the nodes where the creator
still holds it, which bounds everything it later launches. Other conversations carry
it too, so that an automatic agent may only reach a native node with it.

## Cairn hooks

Cairn emits four hooks for the conversations bound to a key, to the rules of the
key's creator that listen to them (`{type: "cairn", hook}`), while that creator keeps
access to the conversation's agent:

| Hook | When | Payload, besides `key`, `conversationId`, `agentId`, `origin` and `values` |
| --- | --- | --- |
| `replyReady` | a turn completed | `text` (the agent's last message, at most 128 KiB, `truncated` when cut) and `deliverables` (at most 10: `id`, `name`, `title`, `mediaType`, `size`) |
| `questionAsked` | the agent asks, once per question still pending, also when its turn already ended; before that turn's `replyReady` and `ended` | `questionId`, `title`, `text` (every question with its options) and `fields` |
| `progress` | about every 8 seconds of a working turn | `step`, a short step without detail |
| `ended` | a turn completed, failed or was cancelled, also when its run stops without the turn reporting it (a queued wakeup cancelled before it started) | `outcome` and, for a failure, `error` (the turn's or the run's, such as an exceeded time limit) |

A turn starts when Cairn delivers its input or wakes the conversation, whatever its
provider notifies. `origin` says who started the turn: `{kind: "person", accountId}`,
`{kind: "rule", ruleId, accountId}`, `agent` (a delegated task), `installation`
(`cairn_installation`), `resume` (a turn without new input, such as a wakeup or the
end of a wait) or `unknown`. A turn that fails and is retried reports only how its last
attempt ended. Only one `progress` waits at a time per conversation: while a rule has
not finished handling the previous one, Cairn emits none, nor once the run no longer
works. A hook whose recorded event would exceed the recorded-data budget (a reply near
128 KiB of characters JSON escapes, with large remembered values) is refused whole:
each rule that would have received it counts it as `notAdmitted`, without its payload. Cairn hooks follow the same
per-rule capacity, versions, membership checks and history as plugin events.

## Versions

Saving creates an immutable version. A triggering keeps the version it started with;
an edit applies to later events only. Each version records who saved it, from where
(`app`, `publicMcp`, `installationAgent`, `duplicate` or `template`), the conversation
an agent wrote it in, and the native provenance of the saving request. A rule reaches
an agent that may run natively, or a native conversation, only if its creator held
native authority in that request ([ADR-0050](adr/0050-trusted-native-execution.md)):
saving a rule that starts an agent restricted to native nodes needs it at once.

## Authority

A rule acts with its creator's authority and belongs to the creator's membership.

| Who | May |
| --- | --- |
| Its creator | edit, enable, disable, duplicate, delete, resume its budget |
| The owner | see every rule, pause or resume any rule in an emergency, resume a budget, delete any rule, duplicate |
| Another member | see the rule and its state, duplicate it with access to every agent it uses |

A duplicate belongs to whoever duplicates it and starts disabled. Only the creator
edits a rule, the owner included.

A rule triggers only when nothing blocks it. Each block lifts independently:

| Block | Cause | Lifted by |
| --- | --- | --- |
| `disabled` | its creator, or a duplication | its creator |
| `emergencyPaused` | the owner | the owner |
| `budgetExhausted` | an hourly cap was exceeded | its creator or the owner |
| `rightsLost` | the creator lost access to the execution agent or to a started agent, or the execution agent lost the hook; an agent deleted or merged away | the right returning, or the creator editing the rule |
| `membershipEnded` | the creator left; re-inviting them changes nothing | never: duplicate it |

Rights are evaluated when the rule is read and before each action, so a returning
right lifts `rightsLost` by itself.

Every member sees the state of every rule. The detail of triggerings (payloads and
action results) is visible to the creator, the owner and members with access to the
execution agent; `allowed.viewDetails` says which applies to the reader.

Removing a hook or tool from an execution agent is refused to `cairn_installation`
while a rule of a current account uses it on that agent, resource restrictions
included and whatever the rule's state. The owner may still reduce it from the apps or
the public MCP: removing the hook blocks the rule (`rightsLost`), and an action whose
tool or resource is no longer granted fails when it runs.

### Warnings

When no condition of the tree tests the author, and the rule starts an agent with
plugin rights, MCP connections, several projects, file writes or commands,
delegation, native execution or installation access, the rule carries an
`openToAnyone` warning naming the agent and its rights. A rule that steers or answers
by key warns the same way about the agents of the conversations bound to its creator's
keys. Saving is still allowed. Listing the rules of an agent (`agentId`) also lists
the rules that steer or answer by key while their creator has a conversation of that
agent bound to a key.

## Rule templates

A plugin may declare rule templates in its manifest
([format](PLUGINS.md#rule-templates-adr-0075-ticket-h)): ready-made sets of rules
with parameters (agents, resources, texts, numbers, booleans).
`list_hook_rule_templates` offers the templates of the instances on which an agent
the caller may use holds a grant, with their parameters and rules.

`install_hook_rule_template` takes the instance, the template's name, the parameter
values and optionally `enabled` (default `true`) and `creationId` (retrying returns
the same rules, even after the template changes or disappears; deleting one of the
installed rules requires a new creation identity). It checks the values (unknown,
missing or ill-typed ones are refused, and the installer must have access to every
agent they name), then creates
one rule per template rule through the same validation and authority as
`create_hook_rule`, all or none: a rule that fails names its position (`Rule 2: …`).
The rules belong to the installer, record the `template` source and keep no link to
the template: a plugin update changes the template, never the rules it created. Each
rule carries its warnings: the installer is warned like any creator when a rule
starts an agent with broad rights without testing the author.

## Surfaces

The HTTP API lives under `/api/hook-rules`. The same definitions back the public MCP
and `cairn_installation`:

| Operation | HTTP | Scope |
| --- | --- | --- |
| `list_hook_rules` | `GET /hook-rules?agentId&instanceId` | read |
| `get_hook_rule` | `GET /hook-rules/{ruleId}` | read |
| `list_hook_rule_versions` | `GET /hook-rules/{ruleId}/versions` | read |
| `get_hook_rule_catalog` | `GET /hook-rules/catalog?agentId` | read |
| `list_hook_rule_triggerings` | `GET /hook-rules/{ruleId}/triggerings?cursor` | read |
| `get_hook_rule_triggering` | `GET /hook-rule-triggerings/{triggeringId}` | read |
| `validate_hook_rule` | `POST /hook-rules/validate` | read |
| `create_hook_rule` | `POST /hook-rules` | manage |
| `replace_hook_rule` | `PUT /hook-rules/{ruleId}` | manage |
| `set_hook_rule_enabled` | `POST /hook-rules/{ruleId}/enabled` | manage |
| `set_hook_rule_emergency_pause` | `POST /hook-rules/{ruleId}/emergency-pause` | manage |
| `lift_hook_rule_budget` | `POST /hook-rules/{ruleId}/budget` | manage |
| `duplicate_hook_rule` | `POST /hook-rules/{ruleId}/duplicate` | manage |
| `delete_hook_rule` | `DELETE /hook-rules/{ruleId}` | manage |
| `list_hook_rule_templates` | `GET /hook-rules/templates?instanceId` | read |
| `install_hook_rule_template` | `POST /hook-rules/templates/install` | manage |

`validate_hook_rule` checks a definition as saving would and returns problems located
by JSON Pointer into the definition, with warnings. The catalog lists the hooks with
their payload schemas and resource fields, the plugin tools with their input schemas,
each action kind with its JSON Schema, the conditions, operators, Cairn functions,
variables, the agents the caller may designate and the limits. A member is offered
only the agents they may use.

A rule created through the public MCP, which needs the manage scope, belongs to the
owner who authorized the client.

### Agents with installation access

`cairn_installation` reads rules, their versions, the catalog and rule templates, and
validates definitions. It installs a template only in an eligible conversation; that
installation creates ordinary rules and follows the same limits. It writes rules only
for one person who remains, when it writes, the owner or a member with access to the
conversation's agent, in a conversation that meets every provenance condition of
ADR-0075:

- the run belongs to a conversation, never to a task (scheduled, manual or delegated)
  or a mission;
- this person created the conversation from the web, Android or the public MCP, and
  wrote every entry since: messages, steers, answers to questions and edits of a
  queued message, which count as entries of whoever edits. An entry by a rule, an
  agent, `cairn_installation` or another person closes the conversation for good;
- the current turn was launched by one of this person's messages: not a resume, a
  recovery, a wakeup, a wait completion or a rule;
- the agent's instructions and the skills the turn follows were written by people,
  never by an agent, and only this person changed them since the conversation
  started. Every followed skill must have a recorded writer for its exact scope and
  revision; unrecorded skills fail closed. Plugin instructions count as written by the
  owner who granted them; a member's conversation closes when another person changes
  them;
- the conversation was never moved to another agent (an agent merge).

Cairn records who writes each conversation entry, each change to an agent's
instructions (including inherited native defaults) and each skill change through its skill tools (`save_skill`,
`edit_skill_files`, `import_skill`, `update_skill_source`, `delete_skill`; imports count
as written by the importer). Skill attestations bind the exact revision and canonical
directory inside the bundle publication transaction; a later file replacement never
inherits the requester's authorship. Every message must carry an attestation of the person
matching its current content: a write that skipped the record makes the conversation
ineligible. An edit or withdrawal first checks the previous message revision and
permanently records an unknown writer if its attestation is absent or mismatched.
Conversations, instructions and skills written before this record never
qualify. Native instruction documents have their own writer history, and each turn
records the source it follows. A human edit cannot erase an agent, rule or unknown
writer from a source's history.
Every earlier turn's instruction sources remain attached to the conversation, and an
unattested turn closes it permanently. The complete native documents for Codex, Claude
and OpenCode are attributed, including inheritance masks; unrecorded instruction-file
references never qualify. Skill history remains attached to the conversation when later
turns stop selecting a skill. Installation skill reads, listings and archive exports
record their exact revisions and canonical directories; reading an unrecorded or
agent-written skill closes the conversation immediately. Native inheritance and removal changes count as instruction
changes too, and so does the native rewrite a profile save makes when it changes engines:
it belongs to whoever saved the profile. A profile save that moves an engine to another
instruction source, even without changing any text, reaches that agent's conversations
as an entry of whoever saved it. Wakeups and engine switches never launch a qualifying
turn, but the instructions and skills they freeze join the conversation's permanent
history like any launch. Accepting an engine switch also records whoever requested
it as a conversation entry, before asynchronous recovery: another person, an agent
or an unknown requester closes the conversation even after a return to the original
engine. Listing, asking or cancelling side questions through `cairn_installation`
always counts as an agent entry, including in the calling conversation: these
operations can return retained text or answers without a writer attestation, even
on an idempotent retry. Removing those questions does not restore eligibility. Codex compaction prompt files
(`experimental_compact_prompt_file`) cannot be configured, like instruction files.
Reading another conversation, run or task through `cairn_installation` (`list_chats`,
`get_chat`, `read_chat_content`, `list_runs`, `get_run`, `read_run_content`, `get_task`)
and delegating a task, whose report the conversation reads, count as agent entries.
The dispatcher defaults every operation not on its closed list of operations that
cannot return conversation, run or task content to an agent entry. The list includes
hook rules, agent profiles, projects, skills, settings, MCP connections, plugins and
nodes; a newly added operation therefore closes the conversation until it is reviewed.
Withdrawing a queued message counts as an entry of whoever withdraws it. An agent's
skill write is recorded before it happens, so a skill an agent ever wrote never
qualifies.

In such a conversation the agent creates, replaces, duplicates, enables, disables and
deletes this person's rules only, saved with the source `installationAgent` and the
conversation's identifier, and never with native authority.
Installing a template through `cairn_installation` follows the same limits. While a
rule of this person is paused in an emergency or out of budget, the agent only disables
their rules. It never pauses or resumes a rule in an emergency, never resumes a budget,
never duplicates a paused or exhausted rule and never touches another account's rule.
When it saves a rule carrying the `openToAnyone` warning, the person receives an alert
with that warning.

`cairn_installation` never reduces a hook or tool grant used by a rule of another
current account (the owner, or a member whose membership lasts), whatever the rule's
state. In an eligible conversation the person's own rules no longer protect a grant;
outside one, every current account's rules do.
