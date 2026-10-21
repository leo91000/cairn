-- Ingress calls this only after original-body host signature verification.
local api = require("./lib/api")

local function response(status, body)
    return { status = status, body = body, headers = { ["content-type"] = "text/plain; charset=utf-8" } }
end

local function identifier(value)
    return type(value) == "string" and #value <= 80 and string.match(value, "^[A-Z][A-Z0-9]+$") ~= nil
end

local function timestamp(value)
    return type(value) == "string" and #value <= 32 and string.match(value, "^%d+%.%d+$") ~= nil
end

local function conversationId(value)
    if type(value) ~= "string" then
        return false
    end
    local a, b, c, d, e = string.match(value, "^(%x+)%-(%x+)%-(%x+)%-(%x+)%-(%x+)$")
    return a ~= nil and #a == 8 and #b == 4 and #c == 4 and #d == 4 and #e == 12
end

return { webhook = function(event, payload)
    if payload.api_app_id ~= event.instance.settings.appId or type(payload.team) ~= "table"
        or not identifier(payload.team.id) or type(payload.user) ~= "table" or not identifier(payload.user.id) then
        return response(403, "Wrong app, workspace or person")
    end
    local container, message, channel = payload.container, payload.message, payload.channel
    local actions = payload.actions
    if type(container) ~= "table" or container.type ~= "message" or container.is_ephemeral == true
        or type(message) ~= "table" or type(channel) ~= "table" or not identifier(channel.id)
        or container.channel_id ~= channel.id or not timestamp(message.ts) or container.message_ts ~= message.ts
        or not timestamp(message.thread_ts) or type(actions) ~= "table" or #actions ~= 1 then
        return response(400, "Invalid source message")
    end
    local action = actions[1]
    if type(action) ~= "table" or action.type ~= "button" or action.action_id ~= "cairn_reset"
        or not timestamp(action.action_ts) or not conversationId(action.value) then
        return response(400, "Invalid control")
    end
    local selected, cursor = nil, nil
    repeat
        local page = cairn.resources.list({ collection = "workspace", cursor = cursor })
        for _, resource in page.items do
            if resource.enabled and resource.settings.externalId == payload.team.id and resource.settings.channelId == channel.id then
                if selected then
                    return response(409, "Ambiguous workspace configuration")
                end
                selected = resource
            end
        end
        cursor = page.nextCursor
    until not cursor
    if not selected then
        return response(403, "Unavailable workspace")
    end
    if message.user ~= selected.settings.botId or payload.user.id == selected.settings.botId
        or (message.app_id ~= nil and message.app_id ~= payload.api_app_id) then
        return response(403, "Wrong source author")
    end
    local id = table.concat({ "reset", payload.team.id, channel.id, message.ts, payload.user.id, action.action_ts }, ":")
    if #id > 200 then
        return response(400, "Invalid control identity")
    end
    local safe, check = api.channel(selected.id, channel.id, payload.team.id)
    if not safe then
        return response(if check.status == 429 or check.status >= 500 then 503 else 403, "Channel unavailable")
    end
    -- Persist only trusted routing and the requested binding, never response_url,
    -- trigger_id, the message contents or unrelated interaction state.
    local result = cairn.inbound.deliver({
        kind = "interaction",
        id = id,
        resource = selected.id,
        author = { id = payload.user.id, name = payload.user.id },
        context = { team = payload.team.id, channel = channel.id, thread = message.thread_ts },
        body = { action = "reset", conversation = action.value },
    })
    return response(if result.status == "busy" then 503 else 200, result.status)
end }
