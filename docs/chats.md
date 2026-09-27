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

Routine session updates (connecting, starting, finishing and success) are hidden from chat, including inside tool groups. The working indicator and completion footer provide that status. Failures and unresolved interruptions appear as inline notices; recovered connection retries disappear. Raw lifecycle events remain in run activity for troubleshooting.

The **Conversations** button opens a searchable conversation selector, grouped by Today, Yesterday and Earlier. It floats below the button on desktop and opens as a bottom sheet on phones. Selecting a conversation keeps each chat’s draft and swaps the conversation in place: the Fil, header and composer stay put, and the previous conversation stays visible (dimmed after a moment) until the selected one is ready, for at most one second before **Loading conversation…** appears. Escape or the close button returns to the current chat. On desktop, the title, agent and project stay above the centered conversation. On phones, one compact toolbar contains **Conversations**, the truncated chat title, **New chat** and **Chat details** (the ellipsis). Chat details opens a bottom sheet with the full title, agent, project, status, Files, notification settings, follow/fullscreen controls, workspace search and navigation, and appearance. The global top bar and separate activity toolbar are hidden only in mobile chat; bottom navigation remains available.

Open **Chats**, or **Start chat** on an agent or project. Project chats select Main agent by default. Choose another agent before the first message; its project, skill, MCP, GitHub and sandbox restrictions still apply. A chat owns one persistent conversation and workspace. It does not create a scheduled task.

Send starts a reply. An initial message appears directly in the conversation with **Sending…**, then **Starting agent…** until the agent accepts it. Steering appears there with **Sending to agent…**. These messages are not counted as queued; the queue panel contains only follow-ups waiting their turn and explicitly says **Queue paused** when paused. Reloading preserves in-flight sends without duplicating acknowledged messages. During a reply, **Queue** saves a follow-up for the next turn, while **Steer now** sends instructions into the active turn. Queued messages can be edited or removed until dispatch begins. Pause queue holds follow-ups without stopping the current reply. Stop response stops execution and pauses the queue; Resume continues the saved conversation. On desktop, Enter sends or queues, Shift+Enter adds a line, and Alt+Enter steers. Touch keyboards use Enter for a line break.

The composer options contain searchable model and reasoning selectors for the next message. They use Codex's live `model/list` catalog, including descriptions and each model's supported reasoning levels. Agent settings use the same selectors. Changing model resets reasoning to that model's default; **Agent default** follows the agent configuration. Saved models absent from the current catalog remain visible. Model and reasoning changes need a new turn; they cannot change an already running turn. Queued edits preserve both choices. Drafts stay in the current browser tab across reloads. Submitted messages, queue state, transcripts and working files persist on the server.

Model discovery is authenticated and cached for five minutes per enabled Codex account. Usage polling refreshes it using the existing account session. The picker combines available models, hides catalog-hidden entries except a saved selection, and offers the reasoning levels shared by accounts exposing that model. Account selection uses fresh catalogs to avoid routing a known model to an account that does not expose it. Discovery failures keep the last catalog with a visible retry status. Custom provider aliases remain supported by the API. [Codex model discovery protocol](https://learn.chatgpt.com/docs/app-server#models).

## Invoking skills with `$`

Type `$` in the composer to list the skills this chat can use: global skills and those of the chat's project that the agent is allowed to access (all accessible projects when the chat has no project). The list filters as you type, matching the name first, then word starts, substrings, letters in order and the description. Each suggestion shows the name, description and Global/project scope. On the web, ↑/↓ move, Enter or Tab inserts and Escape dismisses; clicking works too. On Android, tap a suggestion; Back dismisses the list. The chosen skill is inserted as `$name ` and highlighted in the draft and in sent messages.

The message is stored and displayed exactly as typed. When a reply starts, or a message steers the current turn, the server finds `$name` tokens that match a skill in the run (outside inline and fenced code, not after a letter, `$` or backslash) and appends an `<invoked_skills>` block. That block tells Codex or Claude Code to follow those skills' `SKILL.md` for the request. Tokens like `$HOME`, `$5` or unknown names are left unchanged. The same rules are implemented in `backend/src/skills.rs`, `src/skill-mentions.ts` and `SkillMentions.kt`.

## Execution and recovery

Tasks continue to use `codex exec`. Chats use Codex app-server `thread/start`, `thread/resume`, `turn/start` and `turn/steer`, tested against Codex 0.154.0. Steering includes the active `expectedTurnId`. If that turn has already finished, the message remains pending for the next turn.

`Chats` owns durable user messages and dispatch. `Worker` owns account selection, project locks, timeout budgets, restart fencing and execution. `chat-process.ts` adapts app-server notifications to the shared Activity artifact format. Each active reply has a supervised process; idle chats hold no account or worker slot. Restricted agents run the same adapter inside the isolated container, with a read-only inbox containing only that run's steering messages.

Messages use client-generated IDs. Requests can be retried without duplicating submissions. On recovery, Codex's persisted user-message client IDs reconcile accepted messages, using paginated turn history when required. A completed saved reply is recovered without another model turn. Interrupted replies continue with the existing workspace and conversation. Failures pause follow-ups for review. Usage exhaustion follows the existing account failover path.

SQLite schema version 4 adds indexed chat messages; records and task runs are preserved. Chat events are excluded from the normal 30-day task-event pruning and event-count cap. Workspace cleanup disables further messages in that conversation.

Validation includes authenticated API boundaries, restricted project access, live steering, queue editing and deduplication, model overrides, account-backed isolated execution, restart and lost-completion recovery, process disconnection, and Chromium/WebKit journeys with desktop/mobile light/dark screenshots.

## Questions during a reply

Codex can ask for input while continuing its work, or wait when an answer is required. A **Your input** card appears above the composer, and the chat history shows how many questions need an answer. Open **Answer** to choose an option or write your own response. Suggested choices are never submitted automatically. You can close the panel and answer later; questions survive navigation and server restarts. Answering a live question works even when the follow-up queue is paused.

The adapter enables Codex's `default_mode_request_user_input` feature for chats. It supports `item/tool/requestUserInput` server requests and questions attached to assistant messages. Live request answers use the native response protocol; if the request expired or the process restarted, an answer can instead steer or start the next turn in the same conversation. Other interactive RPC requests, including approvals, remain unavailable. Answer submissions are idempotent and retained until delivery. Questions and the notification outbox are committed together in SQLite.

## Notifications

Open the bell in **Chats** (on phones, **Chat details → Question notifications**), or **Settings → Question notifications**, then choose **Enable on this device**. Permission is requested only after that click. Web Push can deliver while the app is closed; clicking a notification opens the relevant chat and question. Each device can opt out independently. The notification contains no question or answer text.

On iPhone and iPad, install Leo using Safari's **Share → Add to Home Screen**, then enable notifications from that installed app. This requires iOS/iPadOS 16.4 or newer ([WebKit guidance](https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados/)). Delivery depends on the browser, OS notification settings, and network availability.

The public app must use HTTPS. No notification SaaS account is required: VAPID keys are generated automatically and encrypted with the existing persistent application key, along with browser subscription credentials. Keep the data volume and its encryption key when moving the deployment. Supported push providers are Chrome/Chromium (FCM), Firefox, Safari, and Edge's Windows notification service. The server validates provider endpoints, removes expired subscriptions, and retries temporary failures for up to an hour. It skips notifications for questions already answered. The service worker does not cache pages or authenticated API responses.

Tests cover the native Codex request/response shape, blocking and nonblocking questions, late answers, replay, restart, API authorization, notification retry and expiry, private push content, device enrollment/revocation, notification routing, and desktop/mobile question screens in Chromium and WebKit. Browser automation replaces the OS permission prompt and push provider; actual delivery to a phone requires opting in on that phone.
