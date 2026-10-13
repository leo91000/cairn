-- Authoring reference for plugin API v1. Runtime globals are host-owned and frozen.
-- A package's main.luau returns a table of handlers keyed by event name:
--   message           a message from an allowed person      { id, author, content, … }
--   access_requested  someone new asked to use the plugin    { id, author, … } (no content)
--   reply             an agent's final answer to deliver     { deliveryId, text, reply, state }
--                     return { retryAfter = seconds, state = … } to be called again later
--   timer             a timer scheduled with cairn.timers    { id, name, payload }
--   start             when Cairn starts the worker           { connectionId }
--   command           a run-scoped CLI command (built-in plugins only)
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
    -- Adds the connection's credential using the manifest's "credential" rule.
    auth: boolean?,
}

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
    -- capability "timers": 1 second to 7 days, at most 100 per connection.
    timers: {
        after: (seconds: number, name: string, payload: Json?) -> string,
        cancel: (id: string) -> (),
    },
    -- capability "conversations.submit": only while handling `message`, on behalf of
    -- its author. `thread` groups messages into one conversation; `reply` comes back
    -- unchanged in the `reply` event.
    conversations: {
        submit: (message: { text: string, thread: string?, reply: Json? }) -> { chatId: string, messageId: string },
    },
}

return {}
