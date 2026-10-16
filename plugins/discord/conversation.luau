-- What the bot does with admitted messages: one thread and one conversation per
-- request, progress and a Stop button while the agent works, its questions as buttons,
-- and the answer in Discord-sized chunks with the turn's deliverables.
local discord = require("./lib/discord")
local textual = require("./lib/text")

local API, HEADERS, EMOJI = discord.API, discord.HEADERS, discord.EMOJI
local request, post, quietly, react = discord.request, discord.post, discord.quietly, discord.react
local text, truncate, chunks, title = textual.withoutMention, textual.truncate, textual.chunks, textual.title

local WAITING = "Thanks! The owner of this bot needs to approve you first. Mention me again once they have."
local SILENT = "NO_REPLY"
local UPLOAD_LIMIT = 10 * 1024 * 1024
local FAILURES = {
    rate_limited = "I’ve reached my message limit for this hour. Try again a bit later.",
    invalid = "I couldn’t read that message or one of its attachments (10 MB at most each).",
}

local function stopButton(conversation: string): any
    return { {
        type = 1,
        components = { { type = 2, style = 4, label = "Stop", custom_id = "stop:" .. conversation } },
    } }
end

-- Where a reply goes. Unmentioned messages in a support channel get their thread only
-- once the agent decides to answer, so chatter never creates threads.
local function destination(reply: any): string?
    if not reply.deferred then
        return reply.channel
    end
    local key = "thread:" .. reply.message
    local existing = cairn.storage.get(key)
    if existing then
        return existing
    end
    local ok, response = pcall(request, "POST", "/channels/" .. reply.channel .. "/messages/" .. reply.message .. "/threads", {
        name = reply.title or "Support request",
        auto_archive_duration = 1440,
    })
    local thread = if ok and response.status < 300 and response.json then response.json.id else reply.channel
    cairn.storage.set(key, thread)
    return thread
end

-- A question waits for answers to all its fields before it goes back to the agent.
local function questionKey(channel: string, id: string): string
    return "question:" .. channel .. ":" .. id
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
    local closed = ok or (code and string.find(tostring(code), "conflict"))
    if closed then
        cairn.storage.delete(pending.key)
    else
        cairn.storage.set(pending.key, pending)
    end
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
    -- Check each contributor before saving any partial answer. In public mode a
    -- stranger must not influence answers eventually submitted by the starter.
    local allowed = pcall(cairn.conversations.check, { conversation = pending.conversation })
    if not allowed then
        return false
    end
    pending.answers[field.id] = value
    if complete(pending) then
        submitAnswers(channel, pending, who)
    else
        cairn.storage.set(pending.key, pending)
    end
    return true
end

return {
    message = function(event)
        local origin = event.threadId or event.channelId
        local value = text(event.content, event.botId)
        local attachments = event.attachments or {}

        local lowered = string.lower(value)
        if lowered == "stop" or lowered == "cancel" then
            local ok = pcall(cairn.conversations.cancel, { thread = origin, shared = event.threadId ~= nil })
            react(origin, event.id, if ok then EMOJI.stopped else EMOJI.failed, true)
            return nil
        end
        -- Questions are independent, even when several requests share a channel.
        -- Replying to a question message selects it; otherwise use the sole open
        -- question for this thread/person and ask for a reference if ambiguous.
        local pending, candidates = nil, {}
        for _, key in cairn.storage.list("question:" .. origin .. ":") do
            local candidate = cairn.storage.get(key)
            if candidate and event.replyToId and candidate.message == event.replyToId then
                pending = candidate
                break
            end
            if candidate and (event.threadId or candidate.userId == event.author.id) then
                table.insert(candidates, candidate)
            end
        end
        if not pending and #candidates == 1 then
            pending = candidates[1]
        end
        if not pending and #candidates > 1 and value ~= "" then
            quietly(post, origin, {
                content = "Please reply to the specific question message you want to answer.",
                allowed_mentions = { parse = cairn.array() },
            })
            return nil
        end
        if pending and value ~= "" then
            if complete(pending) then
                local allowed = pcall(cairn.conversations.check, { conversation = pending.conversation })
                if allowed then
                    submitAnswers(origin, pending, event.author.name)
                end
                return nil
            end
            for _, field in pending.fields do
                if not field.secret and not pending.answers[field.id] then
                    local recorded = record(origin, pending, field, value, event.author.name)
                    react(origin, event.id, if recorded then EMOJI.done else EMOJI.failed, true)
                    return nil
                end
            end
        end
        if value == "" and #attachments == 0 then
            return nil
        end

        -- In a support channel, messages that don't mention the bot are handled quietly:
        -- no reaction or thread until the agent decides an answer is needed.
        local quiet = event.everyMessage and not event.mentioned and not event.threadId
        local prompt = value
        if event.everyMessage and not event.threadId then
            prompt = "(Message from " .. event.author.name .. " in the Discord support channel. "
                .. "If it needs no answer from you, reply with exactly " .. SILENT .. ".)\n\n" .. value
        end
        local thread = event.threadId
        if quiet then
            -- A thread created from this message later reuses the message's ID.
            thread = event.id
        else
            react(origin, event.id, EMOJI.seen, true)
        end
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
        local reply = if quiet
            then { channel = event.channelId, source = origin, message = event.id, deferred = true, title = title(value) }
            else { channel = thread or event.channelId, source = origin, message = event.id, threaded = thread ~= nil }
        local ok, code = pcall(cairn.conversations.submit, {
            text = prompt,
            files = files,
            thread = thread or event.channelId,
            shared = thread ~= nil,
            reply = reply,
        })
        if not ok and not quiet then
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
        local stopped = string.match(event.customId, "^stop:([%x%-]+)$")
        if stopped then
            pcall(cairn.conversations.cancel, { conversation = stopped })
            return nil
        end
        local id, fieldIndex, optionIndex = string.match(event.customId, "^a:(%x+):(%d+):(%d+)$")
        local pending = if id then cairn.storage.get(questionKey(channel, id)) else nil
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
        -- Quiet requests stay invisible until the agent decides to answer.
        if event.reply.deferred and not cairn.storage.get("thread:" .. event.reply.message) then
            return nil
        end
        local channel = destination(event.reply)
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
                components = stopButton(event.conversation),
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
        local channel = destination(event.reply)
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
                        label = truncate(choice.label, 80),
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
            content = truncate(content, 1900),
            nonce = string.sub(question.id, 1, 24),
            enforce_nonce = true,
            components = if #rows > 0 then rows else cairn.array(),
            allowed_mentions = { parse = cairn.array() },
        })
        if ok and response.status == 429 then
            return { retryAfter = if response.json then response.json.retry_after or 5 else 5 }
        end
        if not ok or response.status >= 300 or not response.json or not response.json.id then
            error("question_unavailable")
        end
        if not secret then
            local key = questionKey(channel, question.id)
            cairn.storage.set(key, {
                key = key,
                id = question.id,
                userId = event.userId,
                deliveryId = event.deliveryId,
                conversation = event.conversation,
                fields = question.fields,
                answers = {},
                message = if ok and response.json then response.json.id else nil,
                content = truncate(content, 1800),
            })
        end
        return nil
    end,

    reply = function(event)
        local key = "status:" .. event.deliveryId
        -- The agent decided this message needs no answer: say nothing.
        if string.match(event.text, "^%s*" .. SILENT .. "%s*$") then
            local status = cairn.storage.get(key)
            if status then
                quietly(request, "DELETE", "/channels/" .. destination(event.reply) .. "/messages/" .. status.id)
                cairn.storage.delete(key)
            end
            react(event.reply.source or event.reply.channel, event.reply.message, EMOJI.seen, false)
            cairn.storage.delete("thread:" .. event.reply.message)
            return { done = true }
        end
        local channel = destination(event.reply)
        local status = cairn.storage.get(key)
        if status then
            quietly(request, "DELETE", "/channels/" .. channel .. "/messages/" .. status.id)
            cairn.storage.delete(key)
        end
        for _, key in cairn.storage.list("question:" .. channel .. ":") do
            local pending = cairn.storage.get(key)
            if pending and pending.deliveryId == event.deliveryId then
                cairn.storage.delete(key)
            end
        end

        local parts = chunks(event.text)
        local uploads, skipped, total = {}, 0, 0
        for _, file in event.files or {} do
            if #uploads < 10 and (file.size or 0) <= UPLOAD_LIMIT and total + (file.size or 0) <= 50 * 1024 * 1024 then
                total += file.size or 0
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
            if index == 0 and not event.reply.threaded and channel == event.reply.channel and not event.reply.deferred then
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
        cairn.storage.delete("thread:" .. event.reply.message)
        return { done = true }
    end,
}
