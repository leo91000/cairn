-- New mail, label changes and deletions as hooks for rules. Gmail pushes only through
-- an owner's Pub/Sub topic, which Cairn does not receive, so a durable timer reads the
-- mailbox history every minute. The first read only sets the starting point.
local GMAIL = "https://gmail.googleapis.com/gmail/v1/users/me"

local INTERVAL = 60
local BACKOFF = 900
local MAX_PAGES = 3
local MAX_DETAILS = 15
local MAX_TEXT = 1000

local settings: any = {}
local generation = 0
local labelNames: any = nil

local function text(value: any, limit: number?): string?
    if type(value) ~= "string" then
        return nil
    end
    local max = limit or MAX_TEXT
    if #value <= max then
        return value
    end
    local cut = utf8.offset(value, 0, max + 1) or (max + 1)
    return string.sub(value, 1, cut - 1)
end

local function encode(value: string): string
    return (string.gsub(value, "[^%w%-_%.~]", function(c)
        return string.format("%%%02X", string.byte(c))
    end))
end

local function get(path: string): (any, number)
    local ok, response = pcall(cairn.http.request, { method = "GET", url = GMAIL .. path, auth = true })
    if not ok then
        return nil, 0
    end
    return if response.status == 200 then response.json else nil, response.status
end

local function emit(name: string, id: string, payload: any)
    pcall(cairn.hooks.emit, { name = name, id = id, payload = payload })
end

local function ids(value: any): { string }
    local list = {}
    for _, id in if type(value) == "table" then value else {} do
        if type(id) == "string" and #id <= 200 and #list < 50 then
            table.insert(list, id)
        end
    end
    return list
end

local function has(list: { string }, wanted: string): boolean
    return table.find(list, wanted) ~= nil
end

-- Custom labels have opaque IDs; their names are read once per worker.
local function names(labels: { string }): any
    if labelNames == nil then
        labelNames = {}
        local answer = get("/labels")
        for _, label in if type(answer) == "table" then answer.labels or {} else {} do
            if type(label.id) == "string" and type(label.name) == "string" then
                labelNames[label.id] = text(label.name, 200)
            end
        end
    end
    local list = {}
    for _, id in labels do
        table.insert(list, labelNames[id] or id)
    end
    return cairn.array(list)
end

-- "Ada Lovelace" <ada@example.com>, as its name and lower-case address.
local function sender(value: string?): (string?, string?)
    if not value then
        return nil, nil
    end
    local name, address = string.match(value, '^%s*"?([^"<]-)"?%s*<([^>]+)>%s*$')
    address = address or string.match(value, "^%s*(%S+@%S+)%s*$")
    if not address or #address > 254 then
        return nil, nil
    end
    name = if name and string.match(name, "%S") then text(name, 200) else nil
    return name, string.lower(address)
end

local function message(id: string, thread: string, labels: { string }, details: boolean): any
    local payload: any = {
        messageId = id,
        threadId = thread,
        labelIds = cairn.array(labels),
        url = "https://mail.google.com/mail/#all/" .. id,
    }
    if not details then
        return payload
    end
    local headers = "&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Cc&metadataHeaders=Subject&metadataHeaders=Date"
    local answer = get("/messages/" .. encode(id) .. "?format=metadata" .. headers)
    if type(answer) ~= "table" then
        return payload
    end
    for _, header in if type(answer.payload) == "table" then answer.payload.headers or {} else {} do
        local name = if type(header.name) == "string" then string.lower(header.name) else ""
        if name == "from" or name == "to" or name == "cc" or name == "subject" or name == "date" then
            payload[name] = text(header.value)
        end
    end
    payload.fromName, payload.fromEmail = sender(payload.from)
    payload.snippet = text(answer.snippet, 500)
    if type(answer.labelIds) == "table" then
        payload.labelIds = cairn.array(ids(answer.labelIds))
    end
    return payload
end

local function load(key: string): any
    local ok, value = pcall(cairn.storage.get, key)
    return if ok and type(value) == "table" then value else {}
end

local function save(key: string, value: any)
    pcall(cairn.storage.set, key, value)
end

-- The mailbox's current position, without anything before it.
local function baseline(state: any): number
    local profile, status = get("/profile")
    if type(profile) ~= "table" or type(profile.historyId) ~= "string" then
        return if status == 401 or status == 403 then BACKOFF else INTERVAL
    end
    state.historyId = profile.historyId
    return INTERVAL
end

local function changes(record: any, budget: any)
    local key = if type(record.id) == "string" then record.id else ""
    for _, added in record.messagesAdded or {} do
        local value = added.message or {}
        local labels = ids(value.labelIds)
        if type(value.id) == "string" and type(value.threadId) == "string" and not has(labels, "DRAFT") then
            local name = if has(labels, "SENT") then "message-sent"
                elseif has(labels, "INBOX") and not has(labels, "SPAM") then "message-received"
                else nil
            if name then
                budget.details -= 1
                emit(name, (if name == "message-sent" then "sent:" else "received:") .. value.id,
                    message(value.id, value.threadId, labels, budget.details >= 0))
            end
        end
    end
    for _, kind in { { "labelsAdded", "label-added", "add" }, { "labelsRemoved", "label-removed", "remove" } } do
        for _, change in record[kind[1]] or {} do
            local value = change.message or {}
            local labels = ids(change.labelIds)
            if type(value.id) == "string" and type(value.threadId) == "string" and #labels > 0 then
                emit(kind[2], "labels:" .. key .. ":" .. value.id .. ":" .. kind[3], {
                    messageId = value.id,
                    threadId = value.threadId,
                    labelIds = cairn.array(labels),
                    labelNames = names(labels),
                })
            end
        end
    end
    for _, deleted in record.messagesDeleted or {} do
        local value = deleted.message or {}
        if type(value.id) == "string" and type(value.threadId) == "string" then
            emit("message-deleted", "deleted:" .. value.id, { messageId = value.id, threadId = value.threadId })
        end
    end
end

-- Reads the history after the saved position, a few pages at a time.
local function read(state: any): number
    if not state.historyId then
        return baseline(state)
    end
    local budget = { details = MAX_DETAILS }
    local start = state.historyId
    local token: string? = nil
    local last: string? = nil
    for _ = 1, MAX_PAGES do
        local path = "/history?maxResults=100&startHistoryId=" .. encode(start)
            .. (if token then "&pageToken=" .. encode(token) else "")
        local answer, status = get(path)
        if status == 404 then
            -- Gmail keeps about a week of history: start again from now.
            state.historyId = nil
            return baseline(state)
        elseif type(answer) ~= "table" then
            -- Records already handled are not read again.
            state.historyId = last or state.historyId
            return if status == 401 or status == 403 then BACKOFF else INTERVAL
        end
        for _, record in answer.history or {} do
            if type(record) == "table" then
                changes(record, budget)
                last = if type(record.id) == "string" then record.id else last
            end
        end
        token = if type(answer.nextPageToken) == "string" then answer.nextPageToken else nil
        if not token then
            if type(answer.historyId) == "string" then
                state.historyId = answer.historyId
            end
            return INTERVAL
        end
    end
    -- Pages still to read resume after the last record handled, a second later.
    state.historyId = last or state.historyId
    return 1
end

local function schedule(delay: number)
    local ok, id = pcall(cairn.timers.after, delay, "watch", { generation = generation })
    if ok then
        save("timer", { id = id })
    end
end

return {
    start = function(event: any)
        settings = event.settings or {}
        local previous = load("watch")
        generation = (tonumber(previous.generation) or 0) + 1
        save("watch", { generation = generation })
        local timer = load("timer")
        if type(timer.id) == "string" then
            pcall(cairn.timers.cancel, timer.id)
        end
        if settings.watchMail ~= false then
            schedule(1)
        end
        return nil
    end,

    timer = function(event: any)
        local payload = event.payload or {}
        if event.name ~= "watch" or payload.generation ~= generation or settings.watchMail == false then
            return nil
        end
        local state = load("history")
        local delay = read(state)
        save("history", state)
        schedule(delay)
        return nil
    end,
}
