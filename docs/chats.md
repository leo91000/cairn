# Agent and project chats

The composer lets you choose **Codex** or **Claude Code** per message. Changing providers preserves the same chat and files, and starts a fresh native session with the visible conversation context. Queued messages retain their provider; the current turn finishes before a switch. See [Claude Code](CLAUDE-CODE.md) for context limits and recovery behavior.

Agents can now publish [persistent deliverables](DELIVERABLES.md): screenshots, videos, audio, PDFs, Markdown, code and downloadable files. Open a card beside a response, or **Files** for the complete gallery. Versions, image comparison and a mobile fullscreen reader remain available after the VM stops.

Chats and task activity use [resumable live streams](STREAMING.md). Multiple browsers can follow the same conversation; refresh and network recovery replay missed events without restarting the agent.

Conversation titles reflect the whole conversation automatically. After a successful reply,
a separate GPT-6 Luna session with `xhigh` reasoning reviews the current title,
all user messages and completed assistant replies in chronological order,
without a message-count limit or truncated bodies. Histories exceeding 64 KB of
serialized input are summarized in ordered segments by the same model; summaries
are combined recursively until they fit. Every segment contributes, including
the beginning and middle of long conversations. Summarization is lossy, but older
exchanges are never simply dropped. Each model request has a two-minute timeout.
It keeps a title that still fits the broader subject and writes a short title in
the conversation's language when the discussion meaningfully changes direction.
Minor follow-ups and acknowledgements should not replace the broader subject. The initial title
remains available immediately; subsequent evaluations are at least five minutes
apart. Pending evaluations coalesce and survive server restarts. Older chats are
evaluated when they receive another completed reply.

This uses an available connected Codex account, including for Claude chats, and
respects that account's capacity and model availability. No separate API key is
needed. Queued Codex work takes priority and cancels a background title request
to release its account slot. The temporary session uses an empty workspace, read-only permissions,
disabled shell/apps/plugins/web search and no conversation tools. Tool output,
reasoning and private question answers are excluded. Failures keep the current
title and retry after the cooldown without interrupting the conversation. A
result made obsolete by newer messages is discarded. Updates reach web and
Android through the existing live stream and do not change the chat's recency.

When an agent reports completion, **Task completed** appears beneath the reply in the conversation. **View evidence** expands the report and supporting details, including clickable commit and workflow links; **Hide evidence** collapses it. Reports without evidence use **View details**. **Blocked** and **Your input needed** show their reason immediately. These are agent-reported outcomes, and a new reply clears the previous report. The composer remains available while reading the evidence.

The conversation reads like the Android app. Your messages sit in a tonal bubble; the agent's replies are plain text under its name. Short conversations sit just above the composer. The agent's actions between two messages collapse into one sentence (for example *Read 2 files, searched once, edited 2 files, ran 2 commands*) with the step count and any failures; expanding it shows a timeline where consecutive reads fold into one step, and each step opens a sheet with its command, output and technical details. While the agent works, the step in progress is left to the working indicator. Deliverables appear as a compact horizontal rail beside the reply. Connection status (**Reconnecting…**, **Updating…**) floats over the conversation instead of moving it. Mission runs use the same reader; see [missions](#missions).

Routine session updates (connecting, starting, finishing and success) are hidden from chat, including inside tool groups. The working indicator and completion footer provide that status. Failures and unresolved interruptions appear as inline notices; recovered connection retries disappear. Raw lifecycle events remain in run activity for troubleshooting.

The **Conversations** button opens a searchable conversation selector, grouped by Today, Yesterday and Earlier. It floats below the button on desktop and opens as a bottom sheet on phones. Selecting a conversation keeps each chat’s draft and swaps the conversation in place: the Fil, header and composer stay put, and the previous conversation stays visible (dimmed after a moment) until the selected one is ready, for at most one second before **Loading conversation…** appears. Escape or the close button returns to the current chat. On desktop, the title, agent and project stay above the centered conversation. On phones, one compact toolbar contains **Conversations**, the truncated chat title, **New chat** and **Chat details** (the ellipsis). Chat details opens a bottom sheet with the full title, agent, project, status, Files, notification settings, follow/fullscreen controls, workspace search and navigation, and appearance. The global top bar and separate activity toolbar are hidden only in mobile chat; bottom navigation remains available.

Open **Chats**, or **Start chat** on an agent or project. Project chats select Main agent by default. Choose another agent before the first message; its project, skill, MCP, GitHub and sandbox restrictions still apply. A chat owns one persistent conversation and workspace. It does not create a scheduled task.

Send starts a reply. An initial message appears directly in the conversation with **Sending…**, then **Starting agent…** until the agent accepts it. Steering appears there with **Sending to agent…**. These messages are not counted as queued. Follow-ups waiting their turn open the composer itself, as on Android: the headline says **Sent when the agent finishes**, or **Queue paused · nothing is sent** with a **Resume** button, and the next message shows with **Now** to steer it into the current turn. **+ N other messages** expands the rest. Selecting a queued message opens its actions: **Send now**, **Edit**, **Pause the queue** or **Resume the queue**, and **Remove from queue**. Reloading preserves in-flight sends without duplicating acknowledged messages. The round button at the end of the composer toolbar follows the Android app: **Send** (arrow) when idle, **Queue** (plus) during a reply or while paused, and **Save** (check) when editing. While the agent works, typing adds a **Steer now** (bolt) button that sends instructions into the active turn, and an empty field shows **Stop response** instead. Queued messages can be edited or removed until dispatch begins. Pause queue holds follow-ups without stopping the current reply. Stop response stops execution and pauses the queue; Resume continues the saved conversation. On desktop, Enter sends or queues, Shift+Enter adds a line, and Alt+Enter steers. Touch keyboards use Enter for a line break.

The composer options contain searchable model and reasoning selectors for the next message. They use Codex's live `model/list` catalog, including descriptions and each model's supported reasoning levels. Agent settings use the same selectors. Changing model resets reasoning to that model's default; **Agent default** follows the agent configuration. Saved models absent from the current catalog remain visible. Model and reasoning changes need a new turn; they cannot change an already running turn. Queued edits preserve both choices. Drafts stay in the current browser tab across reloads. Submitted messages, queue state, transcripts and working files persist on the server.

Model discovery is authenticated and cached for five minutes per enabled Codex account. Usage polling refreshes it using the existing account session. The picker combines available models, hides catalog-hidden entries except a saved selection, and offers the reasoning levels shared by accounts exposing that model. It offers every speed any enabled account lists for that model and marks a speed as `limited` when some signed-in account able to run the model lacks it or cannot confirm it. Account selection uses fresh catalogs to avoid routing a known model to an account that does not expose it. Discovery failures keep the last catalog with a visible retry status. Custom provider aliases remain supported by the API. [Codex model discovery protocol](https://learn.chatgpt.com/docs/app-server#models).

## Speed

Codex models can offer faster **service tiers** in exchange for higher usage. The picker shows a **Speed** row inside the chosen Codex model's card, with **Standard** and every tier that model lists in the live catalog (`model/list` `serviceTiers`, for example `priority`, which Codex names **Fast**). The description under the choices is Codex's own wording for that model and account, such as "1.5x speed, increased usage", followed by a link to [OpenAI's speed page](https://learn.chatgpt.com/docs/agent-configuration/speed). Cairn does not display usage multipliers itself: on 24 October 2026 that page listed Fast at 2.5× and Ultrafast at 8× the Standard rate for included subscription limits, 2× and 6× for purchased credits and Enterprise pay-as-you-go, and API-key billing by tokens; the rates differ per plan and change over time. Ultrafast appears only when the account's catalog offers it (OpenAI limits it to Pro $500, eligible Enterprise and Edu plans).

The speed is independent of the model and the reasoning level: changing either keeps it. **Agent default** / **Conversation default** inherits the agent's or conversation's speed; without one, **Default** is Standard. Codex's app server does not apply a model's catalog `defaultServiceTier` (only Codex's own terminal client does), so Cairn never shows it as the speed in use. Agent settings save a default speed. Each message, including queued ones, keeps the speed chosen when it was written; editing a queued message restores it (an editor that sends no speed keeps it), and steering cannot change the speed of the running turn. Switching to Claude Code or OpenCode drops the speed: they expose no speed setting, and the API rejects a speed for a message whose turn runs with them, whether it names the coding agent or inherits it from the queue or conversation. Messages and agents saved before speeds existed carry none and run at Standard, as they did before.

Codex accepts the tier in `turn/start` and keeps it for the thread, even across `thread/resume`. Cairn therefore sends a tier with every turn: the chosen one, or `default` for Standard and for turns without a speed, so a turn never silently keeps an earlier turn's faster tier. Codex 0.164.0 silently omits a Fast or Ultrafast tier its catalog does not advertise for the model (it passes `flex` through). Routing prefers accounts whose cached catalog confirms the tier: usage polls refresh each account's catalog before it expires, and for speeds a catalog stays authoritative for 30 minutes, since polls skip a busy account. The decision belongs to the turn: just before `turn/start`, each attempt asks the Codex process that will run it for its `model/list`, the catalog Codex itself applies. When that list names the model without the tier, the turn asks for Standard and the conversation shows **This turn runs at Standard speed**. When the list cannot be read, or does not name the model exactly (a custom alias, or a dated version Codex may match to a listed model), the turn still asks for the tier and the conversation shows **Speed not confirmed for this turn**: it may run at Standard. A turn without a chosen model is checked against the model Codex reports for the thread, which its configuration may set; routing only uses the catalog default to order accounts. Codex answers `model/list` with its built-in list, which offers Fast for its main models, when it cannot fetch the account's, and says nothing about it; Cairn's cached account catalogs come from the same call. So a list that names the model without the tier is the account's own and decides: when the process's list or the account's recent cached catalog does so, the turn asks for Standard with the notice above. When both name the tier, the conversation shows **Selected speed requested for this turn**: a request, since the account's plan decides whether it applies, and Cairn does not measure speed. When the account's catalog is unknown or older than 30 minutes, the turn shows **Speed not confirmed for this turn** instead. Notices are recorded only for an attempt Codex accepted. A retried or resumed attempt, possibly on another account, checks again, and the conversation only shows a change of outcome, such as the selected speed after an earlier attempt ran at Standard. A speed must be a `serviceTiers` id from the catalog or `standard`; Codex's list does not name another id, such as its own `fast` alias, so Cairn asks for Standard for it and the notice says so. The picker marks a speed *limited* when some account able to run the model lacks or does not confirm it; the picker label then says the speed is *requested*, as it does when the model's speeds are unknown. Queued messages and the Android agent overview always call a faster speed *requested*, adding *not listed for this model* when the model's catalog lacks it, since each turn reports how it runs. A run without a connected account uses the local Codex catalog the same way, and the speed Cairn sends replaces a `service_tier` set in the host's `~/.codex/config.toml`, which such runs otherwise copy: choose the speed in Cairn instead.

## Invoking skills with `$`

Type `$` in the composer to list the skills this chat can use: global skills and those of the chat's project that the agent is allowed to access (all accessible projects when the chat has no project). The list filters as you type, matching the name first, then word starts, substrings, letters in order and the description. Each suggestion shows the name, description and Global/project scope. On the web, ↑/↓ move, Enter or Tab inserts and Escape dismisses; clicking works too. On Android, tap a suggestion; Back dismisses the list. The chosen skill is inserted as `$name ` and highlighted in the draft and in sent messages.

The message is stored and displayed exactly as typed. When a reply starts, or a message steers the current turn, the server finds `$name` tokens that match a skill in the run (outside inline and fenced code, not after a letter, `$` or backslash) and appends an `<invoked_skills>` block. That block tells Codex or Claude Code to follow those skills' `SKILL.md` for the request. Tokens like `$HOME`, `$5` or unknown names are left unchanged. The same rules are implemented in `crates/installation/src/skills.rs`, `apps/web/src/skill-mentions.ts` and `SkillMentions.kt`.

## Execution and recovery

All executions, including missions, use the chat protocol. Codex uses app-server `thread/start`, `thread/resume`, `turn/start` and `turn/steer`, tested against Codex 0.154.0. Steering includes the active `expectedTurnId`. If that turn has already finished, the message remains pending for the next turn.

`Chats` owns durable user messages and dispatch. `Worker` owns account selection, project locks, timeout budgets, restart fencing and execution. `chat-process.ts` adapts app-server notifications to the shared Activity artifact format. Each active reply has a supervised process; idle chats hold no account or worker slot. Restricted agents run the same adapter inside the isolated container, with a read-only inbox containing only that run's steering messages.

Messages use client-generated IDs. Requests can be retried without duplicating submissions. On recovery, Codex's persisted user-message client IDs reconcile accepted messages, using paginated turn history when required. A completed saved reply is recovered without another model turn. Interrupted replies continue with the existing workspace and conversation. Failures pause follow-ups for review. Usage exhaustion follows the existing account failover path.

SQLite schema version 4 adds indexed chat messages; records and task runs are preserved. Chat events are excluded from the normal 30-day task-event pruning and event-count cap. Workspace cleanup disables further messages in that conversation.

Validation includes authenticated API boundaries, restricted project access, live steering, queue editing and deduplication, model overrides, account-backed isolated execution, restart and lost-completion recovery, process disconnection, and Chromium/WebKit journeys with desktop/mobile light/dark screenshots.

## Questions during a reply

Codex, Claude Code and OpenCode use the same `cairn_workspace.ask_questions`
interface. It creates a questionnaire and immediately returns its ID. The agent
can continue independent work, or report `needs_input` and end its turn while it
waits. A **Your input** card opens the questionnaire on web; Android presents the
same choices in its native dialog.

Each question defaults to single choice, or can allow multiple choices with
optional `minChoices` and `maxChoices`. A comment accompanies selected options;
text alone is also accepted. Bounds apply to checked options: a comment cannot
replace a missing choice. Recommendations are visible and never preselected.
Every question needs a response before the group is submitted in one operation.

Questionnaires survive navigation, restarts, turn completion and coding-agent
changes. A `completed` outcome retires unanswered questionnaires; `needs_input`
keeps them. Manual withdrawal supplies neither an answer nor approval. Answers
steer an active turn or start a continuation, including when the ordinary queue
is paused. Selected option IDs and labels remain separate from the comment in
the message the agent receives. Submitted answers are immutable; corrections use
a new chat message. Repeated creation and submission requests are idempotent.

Native question tools are disabled or rejected. Pre-change pending questionnaires
are retired at startup without rewriting historical messages. Questions and their
notification outbox entries are committed together. See the [questionnaire
contract and examples](USER-QUESTIONS.md) and [ADR-0074](adr/0074-common-agent-question-tool.md).

## Side questions

While Claude Code works on a reply, you can ask a quick explanatory question
without steering it. On the web, use the quote icon in an agent reply's header
(**Ask about this reply**) or select a passage in the reply and choose **Ask about
selection**. On Android, use the **Poser une question à côté** icon or **Demander à côté**
in the text-selection menu. The question opens a side panel (a drawer on desktop, a
bottom sheet on phones and Android) where the answer appears, linked back to its reply:
**Show reply** scrolls the conversation to it (or, for a reply outside the loaded
history, reopens the panel and says so), and each reply shows how many side
questions it has. A card above the composer reports a pending or unread answer.

A side question never joins the queue, steers the turn, pauses or resumes it, or
enters the conversation's history or native session. It is answered beside the
running turn, from what the engine already knows, without tools or file access:
explain a decision, a term or a step, not "check this file". Answers that would need
tools say so and suggest asking in the conversation. Pending questions can be
withdrawn; withdrawal does not affect the reply. Up to five can wait at once.

| Coding agent | Side questions |
| --- | --- |
| Claude Code | The streaming CLI's `side_question` control request (the same one as `/btw`), withdrawn with `control_cancel_request`. Claude Code denies every tool before it could run, answers in one response from the live conversation (an unfinished tool call is marked as still in progress) and skips its transcript. It still runs `PreToolUse` hooks before that denial; Cairn configures none and ignores user and project settings, so only an operator's managed-policy hooks could run. |
| Codex | Unavailable. Its side conversations (`thread/fork`) keep tools Cairn cannot remove before the model runs, such as code-mode shell commands and subagents, and stopping a fork once a tool starts does not undo its effect ([ADR-0078](adr/0078-native-side-questions.md)). |
| OpenCode | Unavailable: it has no side mechanism. |

Codex and OpenCode conversations never offer the action, nor does a Claude reply
that is starting, stopping or compacting, one run by a Claude Code release before
2.1.308, or a conversation between replies. The
side-question panel, when its earlier questions open it, and the API and tools
(`sideQuestionReason`) say why. No second coding agent with write access is started,
and the conversation's own transcript never contains the side question.

The adapter of the running turn announces when its native session can answer a
side question. Before that (while starting, and again after a resume, retry or
recovery) and between replies, the action is unavailable and the API returns 409
with the reason. Adapters from earlier releases never announce it, so their inbox
never contains side questions they could mistake for steering. Side questions never
delay the reply: when it ends, questions still being answered are withdrawn and,
like those still pending, marked unanswered with an explanation. Retried submissions
are idempotent by their UUID. The answer uses the subscription of the account running
the turn, and secrets masked in the conversation are masked in it too. A conversation
keeps its 30 latest side questions, at most 5 waiting at once, with answers of up to
20,000 characters. A reply holds at most 10 open side questions at once: those waiting
and those withdrawn while it runs. A question about a whole reply reaches the agent with
the first 2,000 characters of that reply. Clients identify a reply by its stored
event as well as its native item ID, so an ID reused in a later turn does not select
the later reply. Earlier clients can omit the event and use the item-ID lookup;
if that ID appears in multiple turns, the request is refused until it identifies an event.

## Notifications

Open the bell in **Chats** (on phones, **Chat details → Question notifications**), or **Settings → Question notifications**, then choose **Enable on this device**. Permission is requested only after that click. Web Push can deliver while the app is closed; clicking a notification opens the relevant chat and question. Each device can opt out independently. The notification contains no question or answer text.

On iPhone and iPad, install Cairn using Safari's **Share → Add to Home Screen**, then enable notifications from that installed app. This requires iOS/iPadOS 16.4 or newer ([WebKit guidance](https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados/)). Delivery depends on the browser, OS notification settings, and network availability.

The public app must use HTTPS. No notification SaaS account is required: VAPID keys are generated automatically and encrypted with the existing persistent application key, along with browser subscription credentials. Keep the data volume and its encryption key when moving the deployment. Supported push providers are Chrome/Chromium (FCM), Firefox, Safari, and Edge's Windows notification service. The server validates provider endpoints, removes expired subscriptions, and retries temporary failures for up to an hour. It skips notifications for questions already answered. The service worker does not cache pages or authenticated API responses.

Tests cover common questionnaire creation, native-tool rejection, choices with comments, late answers, replay, restart, API authorization, notification retry and expiry, private push content, device enrollment/revocation, notification routing, and desktop/mobile question screens in Chromium and WebKit. Browser automation replaces the OS permission prompt and push provider; actual delivery to a phone requires opting in on that phone.

## Missions

Missions follow the Android app. The list shows one card per mission: its agent and
project, its name, the schedule in words (for example *Every Monday · 09:00*, or *Paused ·*
before it) with the next date, and a strip of its last twelve runs in order (green
succeeded, coral failed, accent running). A mission needing review says why: **Failed**,
**Interrupted**, **Blocked** or **Your input needed**. The round button runs the mission;
while it runs, the button shows the elapsed time instead. Running missions come first, then
those needing review, then the next scheduled ones, then the rest by name.

The title summarises how many missions are active and when the next one runs. **Search
missions** filters by name and tags; the chips filter **All**, **Scheduled**, **One-off**,
**Paused** and **Archived**, each with its count. **New mission** opens the editor.

Selecting a card opens the mission: beside the list on wide screens, in a bottom sheet on
screens up to 900 px. It shows the agent and project, the name and tags, the schedule with
its time, cron expression, timezone and next two dates, the brief (three lines, expandable),
and the last ten runs with their success rate. Each run opens its own page. The detail ends with
**Edit mission**, **Pause schedule** / **Resume schedule** and **Run now**; the actions menu
duplicates or archives the mission (both paused), opens the latest run, or deletes it. The
selected mission stays in the URL across reloads.

**Run now** opens the new run, as on Android. A run page opens on its **Conversation**, then
offers **Result**, **Files** and **Mission brief**. The conversation reads like a chat: the
agent's actions between messages collapse into one sentence, expand into a timeline and
open each step's command, output and details in a sheet.
