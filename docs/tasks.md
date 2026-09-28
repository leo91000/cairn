# Missions

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
open each step's command, output and details in a sheet; see [chats](chats.md).
