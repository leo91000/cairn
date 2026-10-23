local api = require("./lib/api")
local messages = require("./lib/messages")
local text = require("./lib/text")

local function plain(value, limit)
    return { type = "plain_text", text = text.literal(value, limit or 3000), emoji = false }
end

local function reference(value)
    return type(value) == "string" and #value <= 200
        and (string.match(value, "^question:[%x%-]+:[%x%-]+$") ~= nil
            or string.match(value, "^progress:[%x%-]+$") ~= nil)
end

local function retry(response)
    return { retryAfter = math.min(tonumber(response.headers["retry-after"]) or 5, 300) }
end

local function supported(fields)
    if type(fields) ~= "table" or #fields == 0 or #fields > 3 or #cairn.json.encode(fields) > 16000 then
        return false
    end
    for _, field in fields do
        if field.secret or type(field.options) ~= "table" or #field.options == 0 or #field.options > 5 then
            return false
        end
        for _, option in field.options do
            if type(option.label) ~= "string" or option.label == "" or #option.label > 10000 then
                return false
            end
            local _, complete = text.literal(option.label, 75)
            if not complete then
                -- Truncated labels can make distinct choices indistinguishable.
                return false
            end
        end
    end
    return true
end

local function selected(state, field, option)
    local entry = state.fields[field]
    local choice = if entry and entry.options then entry.options[option] else nil
    if choice and type(choice.label) == "string" then
        return entry.id, choice.label
    end
    return nil, nil
end

local function source(action, resource, payload)
    if not reference(action.value) then
        return nil
    end
    local state = cairn.storage.get(action.value)
    if not state or state.status ~= "sent" or state.resource ~= resource.id
        or state.team ~= payload.team.id or state.channel ~= payload.channel.id
        or state.thread ~= payload.message.thread_ts or state.message ~= payload.message.ts
        or state.userId ~= payload.user.id then
        return nil
    end
    if action.action_id == "cairn_stop" and action.block_id == state.key .. ":stop" then
        return { action = "stop", reference = state.key }
    end
    if not state.question or type(action.action_id) ~= "string" then
        return nil
    end
    local field, option = string.match(action.action_id, "^cairn_answer_(%d+)_(%d+)$")
    field, option = tonumber(field), tonumber(option)
    if not field or not option or action.block_id ~= state.key .. ":" .. tostring(field)
        or action.action_id ~= "cairn_answer_" .. tostring(field) .. "_" .. tostring(option) then
        return nil
    end
    local id = selected(state, field, option)
    return if id then { action = "answer", reference = state.key, field = field, option = option } else nil
end

return {
    source = source,
    progress = function(event)
        if #cairn.storage.list("question:" .. event.deliveryId .. ":") > 0 then
            return nil
        end
        local key = "progress:" .. event.deliveryId
        assert(reference(key), "Invalid native progress reference")
        local state = cairn.storage.get(key)
        if not state then
            state = {
                key = key,
                status = "preparing",
                delivery = event.deliveryId,
                conversation = event.conversation,
                userId = event.userId,
                resource = event.reply.resource,
                team = event.reply.workspace,
                channel = event.reply.channel,
                thread = event.reply.thread_ts,
                readyAt = os.time() + 8,
            }
            -- Leave short work to its final answer. This deadline also survives
            -- a replacement worker, unlike a stored process-local clock.
            cairn.storage.set(key, state, { delivery = state.delivery })
            return nil
        end
        if event.state ~= "working" or os.time() < state.readyAt then
            return nil
        end
        local label = "Working — " .. (if type(event.step) == "string" then event.step else "Thinking")
        local updating = state.status == "sent"
        if updating then
            if state.label == label or state.updateStatus == "sending" then
                return nil
            end
        elseif state.status ~= "preparing" then
            return nil
        end
        if messages.ready(event.reply) then
            return nil
        end
        local safe = api.channel(state.resource, state.channel, state.team)
        if not safe then
            return nil
        end
        local blocks = {
            { type = "section", text = plain(label) },
            { type = "actions", block_id = key .. ":stop", elements = cairn.array({
                { type = "button", action_id = "cairn_stop", value = key, text = plain("Stop"), style = "danger" },
            }) },
        }
        if updating then
            -- Keep the confirmed Stop binding usable while an edit is in flight.
            -- An unconfirmed edit is not automatically repeated after restart.
            state.updateStatus = "sending"
        else
            state.status = "posting"
        end
        cairn.storage.set(key, state, { delivery = state.delivery })
        local values = {
            channel = state.channel,
            text = text.literal(label, 3000),
            blocks = cairn.json.encode(cairn.array(blocks)),
            parse = "none",
        }
        if updating then
            values.ts = state.message
            values.as_user = "true"
        else
            values.thread_ts = state.thread
            values.mrkdwn = "false"
            values.unfurl_links = "false"
            values.unfurl_media = "false"
        end
        messages.before(event.reply)
        local result = api.request("POST", if updating then "chat.update" else "chat.postMessage", values,
            { resource = state.resource, secret = "botToken", prefix = "Bearer " }, state.team)
        messages.after(event.reply)
        if result.status == 429 then
            if updating then
                state.updateStatus = nil
            else
                state.status = "preparing"
            end
            cairn.storage.set(key, state, { delivery = state.delivery })
            return nil
        end
        if result.status ~= 200 or type(result.json) ~= "table" or result.json.ok ~= true
            or result.json.channel ~= state.channel or type(result.json.ts) ~= "string"
            or (updating and result.json.ts ~= state.message) then
            -- A committed but unconfirmed post must not become a second message.
            error("Slack did not confirm the progress message. Check the thread and use Cairn to stop the work.")
        end
        state.status = "sent"
        state.message = result.json.ts
        state.label = label
        state.updateStatus = nil
        cairn.storage.set(key, state, { delivery = state.delivery })
        return nil
    end,
    question = function(event)
        local key = "question:" .. event.deliveryId .. ":" .. event.question.id
        assert(reference(key), "Invalid native question reference")
        local state = cairn.storage.get(key)
        if state and state.status == "sent" then
            return nil
        end
        if state and state.status == "posting" then
            error("Slack question outcome is uncertain. Answer the question in Cairn; check the thread before retrying.")
        end
        local remaining = messages.ready(event.reply)
        if remaining then
            return { retryAfter = remaining }
        end
        if not state then
            state = {
                key = key,
                status = "preparing",
                delivery = event.deliveryId,
                conversation = event.conversation,
                question = event.question.id,
                userId = event.userId,
                resource = event.reply.resource,
                team = event.reply.workspace,
                channel = event.reply.channel,
                thread = event.reply.thread_ts,
                fields = if event.question.blocking and supported(event.question.fields) then event.question.fields else {},
                answers = {},
            }
            -- Reserve the bounded state before provider I/O. Secret questions
            -- keep no fields, prompts, choices or answers in plugin storage.
            cairn.storage.set(key, state, { delivery = state.delivery })
        end
        local safe, check = api.channel(state.resource, state.channel, state.team)
        if not safe then
            if check.status == 429 or check.status >= 500 then
                return retry(check)
            end
            error("The Slack question's channel is no longer available.")
        end
        local lines = { "The agent has a question for the person who started this request." }
        local blocks = { { type = "section", text = plain(lines[1]) } }
        if #state.fields == 0 then
            local fallback = "Answer this question in Cairn. Private answers and questions without supported choices stay there."
            table.insert(lines, fallback)
            table.insert(blocks, { type = "section", text = plain(fallback) })
        else
            for index, field in state.fields do
                table.insert(lines, field.title)
                table.insert(blocks, { type = "section", text = plain(field.title) })
                local buttons = {}
                for option, choice in field.options do
                    table.insert(lines, choice.label)
                    if type(choice.description) == "string" and choice.description ~= "" then
                        table.insert(lines, choice.description)
                        table.insert(blocks, { type = "context", elements = cairn.array({ plain(choice.label .. ": " .. choice.description) }) })
                    end
                    table.insert(buttons, {
                        type = "button",
                        action_id = "cairn_answer_" .. index .. "_" .. option,
                        value = key,
                        text = plain(choice.label, 75),
                    })
                end
                table.insert(blocks, { type = "actions", block_id = key .. ":" .. index, elements = cairn.array(buttons) })
            end
        end
        table.insert(blocks, { type = "actions", block_id = key .. ":stop", elements = cairn.array({
            { type = "button", action_id = "cairn_stop", value = key, text = plain("Stop"), style = "danger" },
        }) })
        state.status = "posting"
        cairn.storage.set(key, state, { delivery = state.delivery })
        messages.before(event.reply)
        local result = api.request("POST", "chat.postMessage", {
            channel = state.channel,
            thread_ts = state.thread,
            text = text.literal(table.concat(lines, "\n\n"), 3000),
            blocks = cairn.json.encode(cairn.array(blocks)),
            mrkdwn = "false",
            parse = "none",
            unfurl_links = "false",
            unfurl_media = "false",
        }, { resource = state.resource, secret = "botToken", prefix = "Bearer " }, state.team)
        messages.after(event.reply)
        if result.status == 429 then
            state.status = "preparing"
            cairn.storage.set(key, state, { delivery = state.delivery })
            return retry(result)
        end
        if result.status ~= 200 or type(result.json) ~= "table" or result.json.ok ~= true
            or result.json.channel ~= state.channel or type(result.json.ts) ~= "string" then
            error("Slack did not confirm the question. Answer it in Cairn; check the thread before retrying.")
        end
        state.status = "sent"
        state.message = result.json.ts
        cairn.storage.set(key, state, { delivery = state.delivery })
        return nil
    end,
    interaction = function(event)
        assert(reference(event.reference), "Invalid question control")
        local state = cairn.storage.get(event.reference)
        assert(state and state.status == "sent" and state.userId == event.author.id
            and state.resource == event.resource and state.team == event.context.team
            and state.channel == event.context.channel and state.thread == event.context.thread,
            "The issued question control is unavailable")
        local target = { conversation = state.conversation, delivery = state.delivery }
        -- A queued callback is not authority: recheck the current execution's
        -- original author, including after a restart, revocation or a new turn.
        cairn.conversations.check(target)
        local safe, check = api.channel(state.resource, state.channel, state.team)
        if not safe then
            if check.status == 429 or check.status >= 500 then
                return retry(check)
            end
            error("The Slack control's channel is no longer available")
        end
        if event.action == "stop" then
            local result = cairn.conversations.cancel(target)
            if state.question then
                cairn.storage.delete(state.key)
            end
            return result
        end
        assert(event.action == "answer" and state.question, "Unknown question control")
        local id, value = selected(state, event.field, event.option)
        assert(id and value, "Unknown issued answer")
        state.answers[id] = value
        for _, field in state.fields do
            if state.answers[field.id] == nil then
                cairn.storage.set(state.key, state, { delivery = state.delivery })
                return { pending = true }
            end
        end
        local result = cairn.conversations.answer({
            conversation = state.conversation,
            delivery = state.delivery,
            question = state.question,
            answers = state.answers,
        })
        cairn.storage.delete(state.key)
        return result
    end,
    cleanup = function(event)
        for _, key in cairn.storage.list("question:" .. event.deliveryId .. ":") do
            cairn.storage.delete(key)
        end
        local key = "progress:" .. event.deliveryId
        local state = cairn.storage.get(key)
        if not state then
            return nil
        end
        if state.status == "sent" then
            local remaining = messages.ready(event.reply)
            if remaining then
                return { retryAfter = remaining }
            end
        end
        -- Retire the control before optional provider cleanup. A failed or
        -- uncertain edit must neither retain controls nor block the final reply.
        cairn.storage.delete(key)
        if state.status == "sent" then
            pcall(function()
                local safe = api.channel(state.resource, state.channel, state.team)
                if not safe then
                    return
                end
                local label = if event.status == "completed" then "Work finished."
                    else if event.status == "cancelled" then "Work stopped." else "Work failed."
                messages.before(event.reply)
                api.request("POST", "chat.update", {
                    channel = state.channel,
                    ts = state.message,
                    text = label,
                    blocks = cairn.json.encode(cairn.array({ { type = "section", text = plain(label) } })),
                    as_user = "true",
                    parse = "none",
                }, { resource = state.resource, secret = "botToken", prefix = "Bearer " }, state.team)
                messages.after(event.reply)
            end)
        end
        return nil
    end,
}
