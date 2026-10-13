-- The Discord bot's behaviour. Cairn's native transport delivers only mentions of this
-- bot, in the configured channel, from allowed people; the host checks every effect.
local API = "https://discord.com/api/v10"
local HEADERS = { ["user-agent"] = "DiscordBot (https://github.com/leo91000/cairn, 1)" }
local WAITING = "Thanks! The owner of this bot needs to approve you first. Mention me again once they have."
local LIMIT = 1900

local function post(channel: string, body: any)
    return cairn.http.request({
        method = "POST",
        url = API .. "/channels/" .. channel .. "/messages",
        headers = HEADERS,
        json = body,
        auth = true,
    })
end

-- Discord counts message length in UTF-16 code units.
local function chunks(text: string): { string }
    local parts, current, units, count = {}, {}, 0, 0
    for _, code in utf8.codes(text) do
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

return {
    message = function(event)
        local text = string.gsub(event.content or "", "<@!?" .. event.botId .. ">", "")
        text = string.match(text, "^%s*(.-)%s*$")
        if text == "" then
            return nil
        end
        cairn.conversations.submit({
            text = text,
            thread = event.channelId,
            reply = { channel = event.channelId, message = event.id },
        })
        return nil
    end,

    access_requested = function(event)
        local key = "notified:" .. event.author.id
        if cairn.storage.get(key) then
            return nil
        end
        cairn.storage.set(key, true)
        post(event.channelId, {
            content = WAITING,
            allowed_mentions = { parse = cairn.array() },
            message_reference = { message_id = event.id, fail_if_not_exists = false },
        })
        return nil
    end,

    reply = function(event)
        local parts = chunks(event.text)
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
            if index == 0 then
                body.message_reference = { message_id = event.reply.message, fail_if_not_exists = false }
            end
            local response = post(event.reply.channel, body)
            if response.status == 429 then
                local seconds = if response.json and response.json.retry_after then response.json.retry_after else 5
                return { retryAfter = seconds, state = { chunk = index } }
            end
            if response.status >= 400 then
                error("rejected")
            end
            index += 1
        end
        return { done = true }
    end,
}
