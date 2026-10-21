local api = require("./lib/api")
local controls = require("./controls")

local function response(status, body)
    return { status = status, body = body, headers = { ["content-type"] = "text/plain; charset=utf-8" } }
end

local function identifier(value)
    return type(value) == "string" and #value <= 80 and string.match(value, "^[A-Z][A-Z0-9]+$") ~= nil
end

local function timestamp(value)
    return type(value) == "string" and #value <= 32 and string.match(value, "^%d+%.%d+$") ~= nil
end

return { webhook = function(event)
    local contentType = string.match(event.headers["content-type"] or "", "^[^;]+")
    if contentType == "application/x-www-form-urlencoded" then
        local encoded = string.match(event.body, "^payload=(.*)$")
        if not encoded or string.find(encoded, "&", 1, true) then
            return response(400, "Invalid interaction form")
        end
        local decoded = string.gsub(encoded, "%+", " ")
        decoded = string.gsub(decoded, "%%(%x%x)", function(byte)
            return string.char(tonumber(byte, 16))
        end)
        local ok, payload = pcall(cairn.json.decode, decoded)
        if not ok or type(payload) ~= "table" or payload.type ~= "block_actions" then
            return response(400, "Invalid interaction")
        end
        return controls.webhook(event, payload)
    end
    local ok, envelope = pcall(cairn.json.decode, event.body)
    if not ok or type(envelope) ~= "table" then
        return response(400, "Invalid event")
    end
    if envelope.type == "url_verification" then
        if type(envelope.challenge) ~= "string" or #envelope.challenge > 1000 then
            return response(400, "Invalid challenge")
        end
        return response(200, envelope.challenge)
    end
    if event.setup or envelope.type ~= "event_callback" then
        return response(200, "Ignored")
    end
    if envelope.api_app_id ~= event.instance.settings.appId or not identifier(envelope.team_id) then
        return response(403, "Wrong app or workspace")
    end
    local message = envelope.event
    if type(message) ~= "table" or message.type ~= "app_mention" or message.subtype ~= nil or message.bot_id ~= nil then
        return response(200, "Ignored")
    end
    if not identifier(message.user) or not identifier(message.channel) or not timestamp(message.ts)
        or (message.thread_ts ~= nil and not timestamp(message.thread_ts)) or type(message.text) ~= "string" or #message.text > 30000 then
        return response(400, "Invalid mention")
    end
    -- Retries are freshly signed, but must not start arbitrarily old work.
    local received = tonumber(event.headers["x-slack-request-timestamp"])
    local sent = tonumber(string.match(message.ts, "^(%d+)"))
    local minutes = event.instance.settings.maxRequestAgeMinutes or 15
    if not received or not sent or received - sent > minutes * 60 or sent - received > 300 then
        return response(200, "Expired request; mention the app again to start fresh")
    end
    local selected, cursor = nil, nil
    repeat
        local page = cairn.resources.list({ collection = "workspace", cursor = cursor })
        for _, resource in page.items do
            if resource.enabled and resource.settings.externalId == envelope.team_id and resource.settings.channelId == message.channel then
                if selected then
                    return response(409, "Ambiguous workspace configuration")
                end
                selected = resource
            end
        end
        cursor = page.nextCursor
    until not cursor
    if not selected or message.user == selected.settings.botId then
        return response(200, "Ignored")
    end
    local mention = "<@" .. selected.settings.botId .. ">"
    if not string.find(message.text, mention, 1, true) then
        return response(200, "Ignored")
    end
    local text = string.gsub(message.text, mention, "")
    text = string.match(text, "^%s*(.-)%s*$")
    if text == "" then
        return response(200, "Empty request")
    end
    -- Slack preserves IDs across visibility changes. Recheck before importing
    -- any content; an unavailable/rate-limited check gets a retry, never admission.
    local safe, check = api.channel(selected.id, message.channel, selected.settings.externalId)
    if not safe then
        return response(if check.status == 429 or check.status >= 500 then 503 else 403, "Channel unavailable")
    end
    local result = cairn.inbound.deliver({
        id = "message:" .. envelope.team_id .. ":" .. message.channel .. ":" .. message.ts,
        resource = selected.id,
        author = { id = message.user, name = message.user },
        context = { team = envelope.team_id, channel = message.channel, thread = message.thread_ts or message.ts },
        body = { text = text },
    })
    return response(if result.status == "busy" then 503 else 200, result.status)
end }
