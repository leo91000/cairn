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
`instanceId`, `receivedAt`), `now` and the item of each enclosing loop. They call the
CEL standard library, the strings and lists extensions and these Cairn functions:
`truncate(text, length)`, `chunks(text, size)`, `formatDate(time, format, timezone?)`,
`lower(text)`, `upper(text)`, `toJson(value)` and `fromJson(text)`. A formula is pure
and its cost is bounded before it runs: at most 2,000 characters and 300 nodes, no
more than two nested `all`/`exists`/`exists_one`/`map`/`filter`, no namespaced
function (such as `lists.range`), no message construction and no optional syntax.

### Conditions

`all` and `any` groups nest up to six levels, with `not`. `compare` applies an operator
to a payload field or formula (`equals`, `notEquals`, `contains`, `startsWith`,
`endsWith`, `matches` a whole regular expression, `in` a list, `present`, `absent`,
`greaterThan`, `lessThan`). `formula` is a boolean CEL expression. Cairn predicates:
`authorIn` (the author is one of the chosen people, as plugin resources),
`authorStartedConversation` (the author started the conversation of a key),
`conversationExists` (a conversation is bound to a key) and `authorQuota` (the author
triggered the rule fewer than `limit` times in `windowMinutes`).

### Actions

`callTool` calls a plugin tool through the execution agent's grants, never a Cairn
tool; its literal inputs are checked against the tool's input schema. `startAgent`
starts an agent the creator may use, optionally bound to a key; it fails when the key
is already bound, and the tree chooses to steer instead. `steerConversation`,
`cancelConversation` and `answerQuestion` reach a conversation only by its key.
`storeValue` remembers a value with the rule, or with the conversation of a key.
`wait` waits from one second to seven days, then resumes the next steps; a loop cannot
wait.

A rule holds at most 200 blocks nested eight levels deep. One triggering runs at most
500 blocks and iterations, waits excluded, and 50 tool calls.

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
the public MCP, which blocks the rule.

### Warnings

When no condition of the tree tests the author, and the rule starts an agent with
plugin rights, MCP connections, several projects, file writes or commands,
delegation, native execution or installation access, the rule carries an
`openToAnyone` warning naming the agent and its rights. Saving is still allowed.

## Surfaces

The HTTP API lives under `/api/hook-rules`. The same definitions back the public MCP
and `cairn_installation`:

| Operation | HTTP | Scope |
| --- | --- | --- |
| `list_hook_rules` | `GET /hook-rules?agentId&instanceId` | read |
| `get_hook_rule` | `GET /hook-rules/{ruleId}` | read |
| `list_hook_rule_versions` | `GET /hook-rules/{ruleId}/versions` | read |
| `get_hook_rule_catalog` | `GET /hook-rules/catalog?agentId` | read |
| `validate_hook_rule` | `POST /hook-rules/validate` | read |
| `create_hook_rule` | `POST /hook-rules` | manage |
| `replace_hook_rule` | `PUT /hook-rules/{ruleId}` | manage |
| `set_hook_rule_enabled` | `POST /hook-rules/{ruleId}/enabled` | manage |
| `set_hook_rule_emergency_pause` | `POST /hook-rules/{ruleId}/emergency-pause` | manage |
| `lift_hook_rule_budget` | `POST /hook-rules/{ruleId}/budget` | manage |
| `duplicate_hook_rule` | `POST /hook-rules/{ruleId}/duplicate` | manage |
| `delete_hook_rule` | `DELETE /hook-rules/{ruleId}` | manage |

`validate_hook_rule` checks a definition as saving would and returns problems located
by JSON Pointer into the definition, with warnings. The catalog lists the hooks with
their payload schemas and resource fields, the plugin tools with their input schemas,
each action kind with its JSON Schema, the conditions, operators, Cairn functions,
variables, the agents the caller may designate and the limits. A member is offered
only the agents they may use.

A rule created through the public MCP, which needs the manage scope, belongs to the
owner who authorized the client.

### Agents with installation access

`cairn_installation` reads rules, their versions and the catalog, and validates
definitions. It writes rules only for one person who remains, when it writes, the
owner or a member with access to the conversation's agent, in a conversation that this
person alone has written since its creation, from the web, Android or the public MCP,
and whose turn this person launched; never in a task (scheduled, manual or delegated)
or a mission. These provenance conditions are listed in ADR-0075. Reading another
conversation through `cairn_installation` (`get_chat`, `read_chat_content`,
`read_run_content`) or a delegated task's report counts as an agent entry. Project
skills written through Cairn's skill tools (`save_skill`, `edit_skill_files`) are
skills written by an agent, not repository content.

Until Cairn records who writes each conversation entry, no conversation meets these
conditions: `cairn_installation` writes no rule. It never grants a right, never lifts
an emergency pause or exhausted budget, never touches another account's rule and never
reduces a grant a rule of a current account uses.
