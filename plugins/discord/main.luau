-- The Discord bot's behaviour. Cairn's native transport delivers only messages for this
-- bot (mentions, and with Message Content its threads), in the configured channel, from
-- allowed people; the host checks every effect.
local API = "https://discord.com/api/v10"
local HEADERS = { ["user-agent"] = "DiscordBot (https://github.com/leo91000/cairn, 1)" }
local WAITING = "Thanks! The owner of this bot needs to approve you first. Mention me again once they have."
local LIMIT = 1900
local UPLOAD_LIMIT = 10 * 1024 * 1024
local EMOJI = {
    seen = "%F0%9F%91%80", -- 👀
    done = "%E2%9C%85", -- ✅
    failed = "%E2%9D%8C", -- ❌
    stopped = "%E2%8F%B9%EF%B8%8F", -- ⏹️
}
local FAILURES = {
    rate_limited = "I’ve reached my message limit for this hour. Try again a bit later.",
    invalid = "I couldn’t read that message or one of its attachments (10 MB at most each).",
}

local function request(method: string, path: string, body: any?)
    return cairn.http.request({ method = method, url = API .. path, headers = HEADERS, json = body, auth = true })
end

local function post(channel: string, body: any)
    return request("POST", "/channels/" .. channel .. "/messages", body)
end

local function quietly(...)
    pcall(...)
end

local function react(channel: string, message: string, emoji: string, on: boolean)
    quietly(request, if on then "PUT" else "DELETE", "/channels/" .. channel .. "/messages/" .. message .. "/reactions/" .. emoji .. "/@me")
end

local function text(content: string, botId: string): string
    local value = string.gsub(content or "", "<@!?" .. botId .. ">", "")
    return string.match(value, "^%s*(.-)%s*$") or ""
end

-- Discord counts message length in UTF-16 code units.
local function chunks(value: string): { string }
    local parts, current, units, count = {}, {}, 0, 0
    for _, code in utf8.codes(value) do
        count += 1
        if count > 16000 then
            break
        end
        local size = if code > 0xFFFF then 2 else 1
        if units + size > LIMIT then
            table.insert(parts, table.concat(current))
            current, units = {}, 0
        end
        table.insert(current, utf8.char(code))
        units += size
    end
    if #current > 0 then
        table.insert(parts, table.concat(current))
    end
    return parts
end

-- Thread names: the first line of the request, within Discord's 100-character limit.
local function title(value: string): string
    local line = string.match(value, "^[^\n]*") or ""
    if line == "" then
        return "Conversation"
    end
    local result, count = {}, 0
    for _, code in utf8.codes(line) do
        count += 1
        if count > 90 then
            table.insert(result, "…")
            break
        end
        table.insert(result, utf8.char(code))
    end
    return table.concat(result)
end

local function stopButton(): any
    return { {
        type = 1,
        components = { { type = 2, style = 4, label = "Stop", custom_id = "stop" } },
    } }
end

-- A question waits for answers to all its fields before it goes back to the agent.
local function questionKey(channel: string): string
    return "question:" .. channel
end

local function complete(pending: any): boolean
    for _, field in pending.fields do
        if field.secret or not pending.answers[field.id] then
            return false
        end
    end
    return true
end

local function submitAnswers(channel: string, pending: any, who: string)
    local ok, code = pcall(cairn.conversations.answer, {
        conversation = pending.conversation,
        question = pending.id,
        answers = pending.answers,
    })
    cairn.storage.delete(questionKey(channel))
    local note = if ok
        then "\n\n✅ Answered by " .. who
        elseif code and string.find(tostring(code), "conflict") then "\n\nThis question was already answered."
        else "\n\nI couldn’t send this answer. Reply in Cairn instead."
    if pending.message then
        quietly(request, "PATCH", "/channels/" .. channel .. "/messages/" .. pending.message, {
            content = pending.content .. note,
            components = cairn.array(),
        })
    end
end

local function record(channel: string, pending: any, field: any, value: string, who: string)
    pending.answers[field.id] = value
    if complete(pending) then
        submitAnswers(channel, pending, who)
    else
        cairn.storage.set(questionKey(channel), pending)
    end
end

return {
    message = function(event)
        local origin = event.threadId or event.channelId
        local value = text(event.content, event.botId)
        local attachments = event.attachments or {}

        if event.threadId then
            local lowered = string.lower(value)
            if lowered == "stop" or lowered == "cancel" then
                local ok = pcall(cairn.conversations.cancel, { thread = event.threadId })
                react(origin, event.id, if ok then EMOJI.stopped else EMOJI.failed, true)
                return nil
            end
            -- A reply while the agent waits on a question answers its first open field.
            local pending = cairn.storage.get(questionKey(event.threadId))
            if pending and value ~= "" then
                for _, field in pending.fields do
                    if not field.secret and not pending.answers[field.id] then
                        record(event.threadId, pending, field, value, event.author.name)
                        react(origin, event.id, EMOJI.done, true)
                        return nil
                    end
                end
            end
        end
        if value == "" and #attachments == 0 then
            return nil
        end

        react(origin, event.id, EMOJI.seen, true)
        local thread = event.threadId
        if not thread then
            -- Each new request gets its own thread; without permission, stay in the channel.
            local ok, response = pcall(request, "POST", "/channels/" .. event.channelId .. "/messages/" .. event.id .. "/threads", {
                name = title(value),
                auto_archive_duration = 1440,
            })
            if ok and response.status < 300 and response.json then
                thread = response.json.id
            end
        end
        local files = {}
        for _, file in attachments do
            table.insert(files, { url = file.url, name = file.name })
        end
        local ok, code = pcall(cairn.conversations.submit, {
            text = value,
            files = files,
            thread = thread or event.channelId,
            shared = thread ~= nil,
            reply = { channel = thread or event.channelId, source = origin, message = event.id, threaded = thread ~= nil },
        })
        if not ok then
            react(origin, event.id, EMOJI.seen, false)
            react(origin, event.id, EMOJI.failed, true)
            local reason = "I couldn’t start this request. The owner can check the bot in Cairn."
            for key, message in FAILURES do
                if string.find(tostring(code), key, 1, true) then
                    reason = message
                end
            end
            quietly(post, thread or event.channelId, {
                content = reason,
                allowed_mentions = { parse = cairn.array() },
            })
        end
        return nil
    end,

    interaction = function(event)
        local channel = event.threadId or event.channelId
        if event.customId == "stop" then
            pcall(cairn.conversations.cancel, { thread = channel })
            return nil
        end
        local id, fieldIndex, optionIndex = string.match(event.customId, "^a:(%x+):(%d+):(%d+)$")
        local pending = cairn.storage.get(questionKey(channel))
        if not id or not pending or pending.id ~= id then
            return nil
        end
        local field = pending.fields[tonumber(fieldIndex) + 1]
        local option = field and field.options and field.options[tonumber(optionIndex) + 1]
        if option and not field.secret then
            record(channel, pending, field, option.label, event.author.name)
        end
        return nil
    end,

    access_requested = function(event)
        local key = "notified:" .. event.author.id
        if cairn.storage.get(key) then
            return nil
        end
        cairn.storage.set(key, true)
        quietly(post, event.threadId or event.channelId, {
            content = WAITING,
            allowed_mentions = { parse = cairn.array() },
            message_reference = { message_id = event.id, fail_if_not_exists = false },
        })
        return nil
    end,

    -- Every few seconds while the agent works: keep "typing…" visible and show the step.
    progress = function(event)
        local channel = event.reply.channel
        quietly(request, "POST", "/channels/" .. channel .. "/typing")
        if event.state ~= "working" or not event.step then
            return nil
        end
        local key = "status:" .. event.deliveryId
        local status = cairn.storage.get(key)
        local content = "⏳ " .. event.step
        if status and status.content == content then
            return nil
        end
        if status then
            quietly(request, "PATCH", "/channels/" .. channel .. "/messages/" .. status.id, { content = content })
            status.content = content
        else
            local ok, response = pcall(post, channel, {
                content = content,
                components = stopButton(),
                allowed_mentions = { parse = cairn.array() },
            })
            if not ok or not response.json then
                return nil
            end
            status = { id = response.json.id, content = content }
        end
        cairn.storage.set(key, status)
        return nil
    end,

    question = function(event)
        local channel = event.reply.channel
        local question = event.question
        local lines, rows, secret = { "**The agent has a question**" }, {}, false
        for index, field in question.fields do
            table.insert(lines, "")
            table.insert(lines, "**" .. field.title .. "**")
            if field.secret then
                secret = true
            elseif field.options and #field.options > 0 and #rows < 5 then
                local buttons = {}
                for option, choice in field.options do
                    if option > 5 then
                        break
                    end
                    table.insert(buttons, {
                        type = 2,
                        style = if #rows == 0 then 1 else 2,
                        label = string.sub(choice.label, 1, 80),
                        custom_id = "a:" .. question.id .. ":" .. (index - 1) .. ":" .. (option - 1),
                    })
                    if choice.description and choice.description ~= "" then
                        table.insert(lines, "• " .. choice.label .. " — " .. choice.description)
                    end
                end
                table.insert(rows, { type = 1, components = buttons })
            else
                table.insert(lines, "_Reply in this thread to answer._")
            end
        end
        if secret then
            table.insert(lines, "")
            table.insert(lines, "Part of this question is private. Answer it in Cairn.")
            rows = {}
        end
        local content = table.concat(lines, "\n")
        local ok, response = pcall(post, channel, {
            content = string.sub(content, 1, 1900),
            components = if #rows > 0 then rows else cairn.array(),
            allowed_mentions = { parse = cairn.array() },
        })
        if not secret then
            cairn.storage.set(questionKey(channel), {
                id = question.id,
                conversation = event.conversation,
                fields = question.fields,
                answers = {},
                message = if ok and response.json then response.json.id else nil,
                content = string.sub(content, 1, 1800),
            })
        end
        return nil
    end,

    reply = function(event)
        local channel = event.reply.channel
        local key = "status:" .. event.deliveryId
        local status = cairn.storage.get(key)
        if status then
            quietly(request, "DELETE", "/channels/" .. channel .. "/messages/" .. status.id)
            cairn.storage.delete(key)
        end
        cairn.storage.delete(questionKey(channel))

        local parts = chunks(event.text)
        local uploads, skipped = {}, 0
        for _, file in event.files or {} do
            if #uploads < 10 and (file.size or 0) <= UPLOAD_LIMIT then
                table.insert(uploads, { artifact = file.id })
            else
                skipped += 1
            end
        end
        if skipped > 0 then
            table.insert(parts, skipped .. " more deliverable" .. (if skipped > 1 then "s are" else " is") .. " available in Cairn.")
        end
        local index = if event.state then event.state.chunk else 0
        local nonce = string.sub((string.gsub(event.deliveryId, "-", "")), 1, 20)
        while index < #parts do
            local body: any = {
                content = parts[index + 1],
                allowed_mentions = { parse = cairn.array() },
                -- Deterministic nonces make retries idempotent within Discord's window.
                nonce = nonce .. tostring(index),
                enforce_nonce = true,
            }
            if index == 0 and not event.reply.threaded then
                body.message_reference = { message_id = event.reply.message, fail_if_not_exists = false }
            end
            local last = index == #parts - 1
            local response
            if last and #uploads > 0 then
                response = cairn.http.request({
                    method = "POST",
                    url = API .. "/channels/" .. channel .. "/messages",
                    headers = HEADERS,
                    multipart = { json = body, files = uploads },
                    auth = true,
                })
                if response.status == 413 then
                    body.content ..= "\n\n(Deliverables were too large for Discord; they are in Cairn.)"
                    response = post(channel, body)
                end
            else
                response = post(channel, body)
            end
            if response.status == 429 then
                local seconds = if response.json and response.json.retry_after then response.json.retry_after else 5
                return { retryAfter = seconds, state = { chunk = index } }
            end
            if response.status >= 400 then
                error("rejected")
            end
            index += 1
        end
        local source, message = event.reply.source or channel, event.reply.message
        react(source, message, EMOJI.seen, false)
        react(source, message, if event.status == "completed" then EMOJI.done
            elseif event.status == "cancelled" then EMOJI.stopped
            else EMOJI.failed, true)
        return { done = true }
    end,
}
