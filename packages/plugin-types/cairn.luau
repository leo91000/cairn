-- Authoring reference for plugin API v2 (v1 packages keep working). Runtime globals
-- are host-owned and frozen.
-- A package's main.luau returns a table of handlers keyed by event name:
--   message           a message from an allowed person      { id, author, content, attachments, … }
--   interaction       a button an allowed person pressed     { id, author, customId, … }
--   access_requested  someone new asked to use the plugin    { id, author, … } (no content)
--   progress          every ~8 s while the agent works       { deliveryId, conversation, state, step, reply }
--   question          the agent asks something, once each    { deliveryId, conversation, question, reply }
--   reply             the agent's final answer to deliver    { deliveryId, conversation, text, status,
--                                                              files, reply, state }
--                     return { retryAfter = seconds, state = … } to be called again later
--   timer             a timer scheduled with cairn.timers    { id, name, payload }
--   start             when Cairn starts the worker           { connectionId, settings }
-- One-shot handlers (API v2), each in a fresh worker:
--   command           `cairn plugin <id> <command> …`        { command, arguments, agent, instances }
--                     return text for `text`/`secret` commands, any JSON otherwise
--   tool              a call of a declared Cairn MCP tool    { tool, input, agent, instances }
--   view              the owner opens an instance's panel    { instance } -> { Component }
--   action            a button or form in that panel         { action, payload, values, instance }
--                     return { notice?, tone?, view? }
--   test              the owner tests an instance            { instance } -> { ok, message? }
-- `instances` are those the agent is granted: { id, name, enabled, settings, secrets }
-- (secret field names only). Pass `instance = id` to capability calls when several.
-- Each handler runs to completion under a deadline and a 16 MB memory limit.
-- Refused host calls raise an error whose message is a short code such as
-- "forbidden", "invalid", "rate_limited" or "unavailable"; use pcall to handle them.

export type Json = nil | boolean | number | string | { Json } | { [string]: Json }

export type Author = { id: string, name: string }

export type HttpRequest = {
    method: ("GET" | "POST" | "PUT" | "PATCH" | "DELETE")?,
    -- HTTPS only, to a host listed in the manifest's "hosts".
    url: string,
    -- content-type, accept, user-agent or x-* only.
    headers: { [string]: string }?,
    json: Json?,
    body: string?,
    -- multipart/form-data: a JSON part plus deliverables of the reply being handled.
    multipart: {
        json: Json?,
        jsonField: string?, -- default "payload_json"
        files: { { artifact: string, field: string? } }?, -- field default "files[n]"
    }?,
    -- true adds the instance's credential using the manifest's "credential" rule; a
    -- table adds a secret field to one header. The script never sees the value.
    auth: (boolean | { secret: string, header: string?, prefix: string? })?,
    -- The instance to act for, in commands and tools granted several.
    instance: string?,
}

-- capability "exec": a program declared in the manifest's "programs", without a shell.
export type ExecRequest = {
    program: string,
    args: { string }?,
    -- Text, or { secret = "<setting key>" } injected by the host.
    env: { [string]: string | { secret: string } }?,
    stdin: string?,
    timeout: number?, -- seconds, 30 at most
    instance: string?,
}

export type ExecResult = { status: number, stdout: string, stderr: string }

-- A view component (API v2). Tones: "neutral" | "info" | "success" | "warning" | "danger".
export type Component = { type: string, [string]: Json }

export type HttpResponse = {
    status: number,
    headers: { [string]: string },
    json: Json?,
    text: string?,
}

export type Cairn = {
    call: (method: string, args: Json) -> Json,
    -- Empty tables encode as JSON objects; wrap tables meant as arrays: cairn.array({}).
    array: <T>(items: { T }?) -> { T },
    -- capability "storage": up to 1000 values of 64 KB per connection.
    storage: {
        get: (key: string) -> Json,
        set: (key: string, value: Json) -> (),
        delete: (key: string) -> (),
        list: (prefix: string?) -> { string },
    },
    -- capability "http".
    http: { request: (request: HttpRequest) -> HttpResponse },
    -- capability "exec" (API v2).
    exec: { run: (request: ExecRequest) -> ExecResult },
    json: { decode: (text: string) -> Json, encode: (value: Json) -> string },
    -- Stops the handler with a message for the person (300 characters at most).
    fail: (message: string) -> never,
    -- capability "timers": 1 second to 7 days, at most 100 per connection.
    timers: {
        after: (seconds: number, name: string, payload: Json?) -> string,
        cancel: (id: string) -> (),
    },
    -- capability "conversations.submit": only while handling `message`, on behalf of
    -- its author. `files` are fetched from declared hosts (4 × 10 MB). With
    -- `shared = true`, everyone allowed who writes in `thread` continues one conversation.
    conversations: {
        submit: (message: {
            text: string,
            thread: string?,
            shared: boolean?,
            files: { { url: string, name: string? } }?,
            reply: Json?,
        }) -> { chatId: string, messageId: string },
        -- capability "conversations.control": while handling `message` or `interaction`.
        -- Name the conversation by ID or by its shared thread.
        -- Check before collecting a partial answer; the final effect also rechecks.
        check: (target: { conversation: string?, thread: string? }) -> { conversation: string },
        answer: (answer: {
            conversation: string?,
            thread: string?,
            question: string,
            answers: { [string]: string },
        }) -> { status: string },
        cancel: (target: { conversation: string?, thread: string?, shared: boolean? }) -> { stopped: boolean },
    },
}

return {}
