-- Authoring reference for plugin API v2 (`"apiVersion": 2`). Runtime globals
-- are host-owned and frozen. A package can be split into modules: `require("./lib/x")`
-- loads lib/x.luau (or lib/x/init.luau) relative to the requiring file, once per worker,
-- from the package's own files only.
-- A package's main.luau returns a table of handlers keyed by event name:
--   message           what an allowed person wrote, admitted { id, author, …context, …body }
--                     with cairn.inbound.accept (or another `kind`, e.g. interaction)
--   access_requested  someone new asked to use the plugin    { id, author, … } (no content)
--   progress          every ~8 s while the agent works       { deliveryId, conversation, state, step, reply }
--   question          the agent asks something, once each    { deliveryId, conversation, question, reply }
--   reply             the agent's final answer to deliver    { deliveryId, conversation, text, status,
--                                                              files, reply, state, checkpoint }
--                     return { retryAfter = seconds, state = … } to be called again later
--   timer             a timer scheduled with cairn.timers    { id, name, payload }
--   start             when Cairn starts the worker           { connectionId, settings }
--   socket            capability "socket", in the transport worker: { type = "start", instance,
--                     settings } | { type = "open" } | { type = "message", text?, oversized? } |
--                     { type = "tick", name } | { type = "close", code?, reason, error? }
--                     (`oversized`: the message's size; `text` then holds only the fields
--                     named in `connect`'s `oversized`)
-- One-shot handlers, each in a fresh worker:
--   command           `cairn plugin <id> <command> …`        { command, arguments, agent, instances }
--                     return text for `text`/`secret` commands, any JSON otherwise
--   tool              a call of a declared Cairn MCP tool    { tool, input, agent, instances }
--   view              the owner opens an instance's panel    { instance } -> { Component }
--   action            a button or form in that panel         { action, payload, values, instance }
--                     return { notice?, tone?, view? }
--   test              the owner tests an instance            { instance } -> { ok, message? }
--   form              the owner edits an instance (instanceForm)
--                     { values, credential, secrets, instance?, changed?, submit, setup? }
--                     setup = {revision, expiresAt, urls = {webhooks: {[name]: url}, oauthCallback}}
--                     appears only for an owner-bound persisted configuration draft
--                     or a collection declaring form=true: {instance, app, resource,
--                     values, changed?, submit}. resource={collection,id?,parent?,secrets};
--                     app/parent contain nonsecret settings; secrets are names only.
    --                     { values, previous?, credential, secrets, instance?, changed?, submit }
--                     -> FormResult; http and exec use the draft's credential and secrets
--   webhook           a verified signed public POST         {endpoint, method, headers, body, instance, setup}
--                     setup=true accepts only declared setup challenges; all host capabilities are refused
--                     -> {status, body: string, headers?: {["content-type"]: string}}
--                     Original UTF-8 body; 128 KiB input, 64 KiB response, 2.5 s total.
--   projects          a `projectSources` request, for the owner through one instance
--                     { op = "list", source, page, query, instance }
--                       -> { items = { { id, name, description?, defaultBranch?, badges?,
--                            updatedAt?, url?, origin? } }, nextPage? }
--                     { op = "resolve", source, id, instance }
--                       -> { id, name, description?, origin, defaultBranch? }
--   condition         a granted agent's `conditions` wait     { condition, input, project?,
--                     agent, instances } -> { state = "waiting", display = { title, short,
--                     details } } | { state = "completed" | …, …, display = { tone, title,
--                     detail } } | { state = "unavailable", message }; errors are retried,
--                     `checkAfter` (seconds) delays the next check
--   skills            a `skillSources` request, for the owner through one instance
--                     { op = "resolve", source, location, instance }
--                       -> { version, location?, folder?, label?, url?, message?, date? }
--                     { op = "files", source, location, version, folder, instance }
--                       -> { files = { { path, size, digest, executable?, link?,
--                            download = { url, headers? } } }, truncated? }
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
    form: { [string]: string }?, -- URL encoded text fields, exclusive with other body types
    body: string?,
    -- capability "files": a private handle, outside the worker's memory limit.
    bodyFile: { id: string, format: "raw" | "json" | "base64url", jsonPointer: string? }?,
    responseFile: { name: string?, mediaType: string?, jsonPointer: string?, encoding: ("base64" | "base64url")? }?,
    -- Durable per-instance mutation receipt. An interrupted request with the same
    -- key is refused until its outcome has been checked with the provider.
    idempotencyKey: string?,
    idempotencyInput: Json?, -- stable intent when MIME boundaries/file handles vary
    -- multipart/form-data: a JSON part plus deliverables of the reply being handled.
    multipart: {
        json: Json?,
        jsonField: string?, -- default "payload_json"
        files: { { artifact: string, field: string? } }?, -- field default "files[n]"
    }?,
    -- true adds the instance's credential using the manifest's "credential" rule; a
    -- table adds a secret field to one header. The script never sees the value.
    auth: (boolean | { secret: string, header: string?, prefix: string?, resource: string? }
        | { resource: string, oauth: string }
        -- Resource form only: relative, host-bound credentials. A new self may use
        -- typed secrets; OAuth needs a saved self or the selected parent. Owner-only.
        | { configuration: "self" | "parent", secret: string, header: string?, prefix: string? }
        | { configuration: "self" | "parent", oauth: string })?,
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

export type PersonalOAuthTarget = { resource: string, provider: string }
export type PersonalOAuthStatus = {
    status: "disconnected" | "pending" | "denied" | "authorized" | "reconnect",
    subject: string?,
    scopes: { string }?,
    requestedScopes: { string }?,
    expiresAt: number?,
}

export type PersonalOAuthConsent = { url: string, redirectUri: string, expiresAt: number }

-- An installer's resolve(input) returns an exact release. Its install(input)
-- receives the same release and returns the opaque executable handle below.
export type ProgramRelease = {
    version: string,
    url: string,
    sha256: string?,
    format: "zip" | "binary",
    executable: string,
}
export type ProgramPlatform = { os: string, arch: string }
export type ProgramResolveInput = { platform: ProgramPlatform }
export type ProgramInstallInput = { platform: ProgramPlatform, release: ProgramRelease }
export type DownloadHandle = { id: string, sha256: string }
export type ExecutableHandle = { id: string }

-- A view component. Tones: "neutral" | "info" | "success" | "warning" | "danger".
export type Component = { type: string, [string]: Json }

export type FieldOption = { value: string, label: string, description: string?, disabled: boolean? }
export type FormLookup = { field: string, query: string, cursor: string? }

-- What the `form` handler returns. `values` may set any declared non-secret setting,
-- hidden ones included. Root errors also allow "credential"/"form"; resource
-- errors allow "parentId"/"form". Resource results use only their collection schema.
export type FormResult = {
    options: { [string]: { FieldOption } }?,
    values: { [string]: Json }?,
    errors: { [string]: string }?,
    view: { Component }?,
    -- Missing page metadata means completeness unknown; use cairn.json.null at the end.
    pages: { [string]: { nextCursor: Json } }?,
}

-- capability "inbound": something an allowed person wrote, received by the package.
export type Inbound = {
    kind: string?, -- handler to call, default "message"
    id: string, -- platform message ID, deduplicated for 30 days
    resource: string?, -- required for collections declaring delegation; host selects its route
    author: Author,
    context: { [string]: Json }?, -- routing the package needs; kept for requests too
    body: { [string]: Json }?, -- what the person wrote; kept only for allowed people
}

export type Resource = {
    id: string,
    connectionId: string,
    collection: string,
    parentId: string?,
    name: string,
    enabled: boolean,
    revision: string,
    settings: { [string]: Json },
    secretKeys: { string }, -- names only; auth.resource injects a value in HTTP
    updatedAt: number,
}

-- A collection declaring `form = true` uses the package's shared form handler.
-- App and parent metadata are nonsecret; credential values remain in the host.
export type ResourceFormInput = {
    instance: string,
    app: { id: string, name: string, enabled: boolean, settings: { [string]: Json }, secrets: { string } },
    resource: { collection: string, id: string?, parent: Resource?, secrets: { string } },
    values: { [string]: Json },
    changed: string?,
    lookup: FormLookup?, -- choice-only preview: no values/view or another field's options/errors
    submit: boolean,
}

export type HttpResponse = {
    status: number,
    -- content-type, retry-after, link and x-* headers.
    headers: { [string]: string },
    json: Json?,
    text: string?,
    file: TransferFile?,
}

export type TransferFile = { id: string, name: string, mediaType: string, size: number, expiresAt: number, messageId: string?, contentDigest: string? }
export type MailAddress = string | { email: string, name: string? }
export type MimeMessage = {
    instance: string?, from: MailAddress, to: { MailAddress }?, cc: { MailAddress }?, bcc: { MailAddress }?, replyTo: { MailAddress }?,
    subject: string?, text: string?, html: string?, messageId: string?, messageKey: string?,
    inReplyTo: { string }?, references: { string }?,
    attachments: { { id: string, name: string?, mediaType: string, contentId: string? } }?,
}

export type Cairn = {
    call: (method: string, args: Json) -> Json,
    -- Empty tables encode as JSON objects; wrap tables meant as arrays: cairn.array({}).
    array: <T>(items: { T }?) -> { T },
    -- capability "storage": up to 1000 values of 64 KB per connection.
    storage: {
        get: (key: string) -> Json,
        set: (key: string, value: Json) -> (),
        -- Atomic maximum of finite numbers; a later observation cannot shorten it.
        max: (key: string, value: number) -> number,
        delete: (key: string) -> (),
        list: (prefix: string?) -> { string },
    },
    -- capability "http".
    http: { request: (request: HttpRequest) -> HttpResponse },
    -- capability "files": one-hour, instance-bound handles. Chunks <= 64 KiB;
    -- files <= 40 MiB; 200 MiB/instance, 1 GiB/installation, 256 handles.
    files: {
        create: (request: { instance: string?, name: string?, mediaType: string?, data: string?, encoding: string? }) -> TransferFile,
        append: (request: { instance: string?, id: string, offset: number, data: string, encoding: string? }) -> Json,
        read: (request: { instance: string?, id: string, offset: number?, limit: number? }) -> Json,
        delete: (request: { instance: string?, id: string }) -> Json,
        composeMime: (request: MimeMessage) -> TransferFile,
    },
    -- capability "exec".
    exec: { run: (request: ExecRequest) -> ExecResult },
    -- Installer install(input) only. Download the exact URL/checksum selected by
    -- resolve; no credentials, arbitrary paths or downloaded bytes enter Luau.
    download: (request: { url: string, sha256: string? }) -> DownloadHandle,
    archive: {
        executable: (request: {
            archive: string, -- DownloadHandle.id
            format: "zip" | "binary",
            path: string, -- the release's executable entry
        }) -> ExecutableHandle,
    },
    -- capability "socket", in the transport worker only. WSS to declared hosts;
    -- `{{secret}}` in sent text becomes the credential (auth = true) or a secret field.
    -- Commands apply once the handler returns.
    socket: {
        -- `after`: seconds to wait first (one day at most). `oversized`: JSON fields such
        -- as "d.id" or "d.threads.*.id" kept from messages over 1 MB. * projects arrays.
        connect: (request: {
            url: string,
            auth: (boolean | { secret: string, header: string?, prefix: string? })?,
            after: number?,
            oversized: { string }?,
        }) -> (),
        -- Queued outside the handler; paced actual writes share the credential's budget.
        -- `name` emits { type = "sent", name, sent = true } after writing, or sent=false
        -- if the budget observation expires or lowers while queued (refetch and retry).
        send: (text: string, options: {
            auth: (boolean | { secret: string })?,
            pace: number?, -- ms, up to 60000
            budget: { remaining: number, resetAfter: number }?, -- resetAfter in ms
            name: string?,
        }?) -> { queued: boolean, retryAfter: number? },
        close: (code: number?) -> (),
        -- Named timers of the current connection, delivered as { type = "tick", name }.
        every: (name: string, ms: number, first: number?) -> (),
        after: (name: string, ms: number) -> (),
        cancel: (name: string) -> (),
    },
    -- capability "inbound": Cairn keeps it only if the person is allowed (approval mode
    -- records a request without content). Returns whether it was queued.
    inbound: {
        accept: (message: Inbound) -> boolean,
        -- Use the precise result before acknowledging external delivery. "busy" is
        -- temporary; "requested" stores identity/routing only, never message content.
        deliver: (message: Inbound) -> {status: "accepted" | "duplicate" | "denied" | "requested" | "busy"},
    },
    -- Pages are bounded by records and encoded bytes; follow every non-nil nextCursor.
    resources: {
        get: (options: {collection: string, resource: string, instance: string?}) -> Resource,
        list: (options: {collection: string, cursor: string?, instance: string?}) -> {items: {Resource}, nextCursor: string?},
    },
    -- capability "oauth": a personal provider, from a resource-routed admitted
    -- message/interaction only. The host supplies the person and route. Send the
    -- URL privately; the same person finishes from an admitted event after
    -- their browser callback. No tokens, account identity or authority in inputs.
    oauth: {
        -- A durable replay keeps its URL, or returns its already completed status.
        start: (options: PersonalOAuthTarget & { scopes: { string } }) -> PersonalOAuthConsent | PersonalOAuthStatus,
        finish: (options: PersonalOAuthTarget) -> PersonalOAuthStatus,
        status: (options: PersonalOAuthTarget) -> PersonalOAuthStatus,
        disconnect: (options: PersonalOAuthTarget) -> PersonalOAuthStatus,
    },
    -- With "socket" or "inbound": the state the owner sees, "connecting" or "connected".
    instance: { status: (state: string, message: string?) -> () },
    -- Explicit null survives in an outgoing object. Incoming null still becomes nil.
    json: { null: Json, decode: (text: string) -> Json, encode: (value: Json) -> string },
    -- Stops the handler with a message for the person (300 characters at most). In the
    -- transport worker it stops the connection until the owner reviews the instance.
    fail: (message: string) -> never,
    -- capability "timers": 1 second to 7 days, at most 100 per connection.
    timers: {
        after: (seconds: number, name: string, payload: Json?) -> string,
        cancel: (id: string) -> (),
    },
    -- During reply only, with conversations.submit: durable state for the host's
    -- current delivery. Available as event.checkpoint on retries, even after an
    -- exception/restart. At most 64 KiB; removed atomically when delivery settles.
    delivery: { checkpoint: (state: Json) -> () },
    -- capability "conversations.submit": only while handling an admitted message, for
    -- its author. `files` are fetched from declared hosts (4 × 10 MB). With
    -- `shared = true`, everyone allowed who writes in `thread` continues one conversation.
    conversations: {
        submit: (message: {
            text: string,
            thread: string?,
            shared: boolean?,
            -- Resource-routed messages only: queue an independent turn. If the
            -- preceding turn stops, do not replay its prompt or native session.
            newTurn: boolean?,
            files: { { url: string, name: string? } }?,
            reply: Json?,
        }) -> { chatId: string, messageId: string },
        -- capability "conversations.control": while handling `message` or `interaction`.
        -- Name the conversation by ID or by its shared thread. Resource-routed
        -- controls require the issuing turn's deliveryId; only its author may act.
        -- Check before collecting a partial answer; the final effect also rechecks.
        check: (target: { conversation: string?, thread: string?, delivery: string? }) -> { conversation: string },
        answer: (answer: {
            conversation: string?,
            thread: string?,
            delivery: string?,
            question: string,
            answers: { [string]: string },
        }) -> { status: string },
        cancel: (target: { conversation: string?, thread: string?, shared: boolean?, delivery: string? }) -> { stopped: boolean },
        -- Resource-routed admitted people only. Reset a settled source thread,
        -- naming its expected conversation; outstanding work/replies must finish.
        -- Cairn keeps its history. The next submit creates a new conversation.
        -- Retrying the same admitted event returns the committed reset receipt.
        reset: (target: { conversation: string, thread: string, shared: boolean? }) -> { reset: boolean, conversation: string },
    },
}

return {}
