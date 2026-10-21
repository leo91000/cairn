local api = require("./lib/api")

-- Reply handlers for an app share one event worker. Luau's os.clock is
-- monotonic on the supported hosts. Prime a fresh worker for one second too,
-- so replacing the worker cannot immediately burst after its predecessor.
local startedAfter = os.clock() + 1
local nextPost = {}

return {
    message = function(event)
        return cairn.conversations.submit({
            text = event.text,
            thread = event.context.team .. ":" .. event.context.channel .. ":" .. event.context.thread,
            shared = true,
            newTurn = true,
            reply = { workspace = event.context.team, resource = event.resource, channel = event.context.channel, thread_ts = event.context.thread },
        })
    end,
    reply = function(event)
        local state = event.checkpoint
        if state and state.status == "sent" then
            return { done = true }
        end
        if state and state.status == "sending" then
            -- Slack can commit a write before a timeout or internal_error. Never
            -- repeat an uncertain post; leave it visibly undelivered for recovery.
            error("Slack reply outcome is uncertain. Check the thread before retrying.")
        end
        local channelKey = event.reply.workspace .. ":" .. event.reply.channel
        local remaining = math.max(startedAfter, nextPost[channelKey] or 0) - os.clock()
        if remaining > 0 then
            return { retryAfter = remaining }
        end
        for key, deadline in nextPost do
            if deadline <= os.clock() then
                nextPost[key] = nil
            end
        end
        local text = (string.gsub(string.gsub(string.gsub(event.text, "&", "&amp;"), "<", "&lt;"), ">", "&gt;"))
        if #text > 30000 then
            error("Slack reply needs chunking before it can be delivered.")
        end
        local safe, check = api.channel(event.reply.resource, event.reply.channel, event.reply.workspace)
        if not safe then
            if check.status == 429 then
                return { retryAfter = math.min(tonumber(check.headers["retry-after"]) or 5, 300) }
            end
            error("The Slack channel is no longer an available public, unshared channel.")
        end
        cairn.delivery.checkpoint({ status = "sending" })
        nextPost[channelKey] = os.clock() + 1
        local result = api.request("POST", "chat.postMessage", {
            channel = event.reply.channel,
            thread_ts = event.reply.thread_ts,
            text = text,
            mrkdwn = "false",
            parse = "none",
            unfurl_links = "false",
            unfurl_media = "false",
        }, { resource = event.reply.resource, secret = "botToken", prefix = "Bearer " }, event.reply.workspace)
        -- Wait from the response as well as from the request. This preserves
        -- a full second between provider arrivals despite different I/O delays.
        nextPost[channelKey] = os.clock() + 1
        if result.status == 429 then
            cairn.delivery.checkpoint({ status = "retryable" })
            return { retryAfter = math.min(tonumber(result.headers["retry-after"]) or 5, 300) }
        end
        if result.status ~= 200 or type(result.json) ~= "table" or result.json.ok ~= true
            or result.json.channel ~= event.reply.channel or type(result.json.ts) ~= "string" then
            error("Slack did not confirm the reply. Check the thread before retrying.")
        end
        cairn.delivery.checkpoint({ status = "sent", ts = result.json.ts })
        return { done = true }
    end,
}
