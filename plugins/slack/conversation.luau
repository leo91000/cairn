local api = require("./lib/api")
local text = require("./lib/text")

-- Reply handlers for an app share one event worker. Luau's os.clock is
-- monotonic on the supported hosts. Prime a fresh worker for one second too,
-- so replacing the worker cannot immediately burst after its predecessor.
local startedAfter = os.clock() + 1
local nextPost = {}

local function controls(event, part)
    return cairn.json.encode(cairn.array({
        { type = "section", text = { type = "plain_text", text = part, emoji = false } },
        { type = "actions", block_id = "cairn-reset:" .. event.deliveryId, elements = cairn.array({
            { type = "button", action_id = "cairn_reset", value = event.conversation,
                text = { type = "plain_text", text = "New conversation", emoji = false },
                confirm = {
                    title = { type = "plain_text", text = "New shared conversation" },
                    text = { type = "plain_text", text = "Retire this settled conversation. The next authorized request in this Slack thread starts fresh; Cairn history stays available." },
                    confirm = { type = "plain_text", text = "Start fresh" },
                    deny = { type = "plain_text", text = "Keep conversation" },
                },
            },
        }) },
    }))
end

return {
    interaction = function(event)
        assert(event.action == "reset", "Unknown Slack control")
        return cairn.conversations.reset({
            conversation = event.conversation,
            thread = event.context.team .. ":" .. event.context.channel .. ":" .. event.context.thread,
            shared = true,
        })
    end,
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
        local chunks = text.chunks(event.text)
        local index = if state and state.nextChunk then state.nextChunk else 1
        assert(type(index) == "number" and index == math.floor(index) and index >= 1 and index <= #chunks,
            "The Slack reply checkpoint is invalid; check the thread before retrying.")
        local safe, check = api.channel(event.reply.resource, event.reply.channel, event.reply.workspace)
        if not safe then
            if check.status == 429 then
                return { retryAfter = math.min(tonumber(check.headers["retry-after"]) or 5, 300) }
            end
            if check.status >= 500 then
                -- No message write was attempted. Keep the accepted reply while
                -- channel metadata is temporarily unavailable.
                return { retryAfter = 5 }
            end
            error("The Slack channel is no longer an available public, unshared channel.")
        end
        local finished = index == #chunks
        cairn.delivery.checkpoint({ status = "sending", nextChunk = index })
        nextPost[channelKey] = os.clock() + 1
        local result = api.request("POST", "chat.postMessage", {
            channel = event.reply.channel,
            thread_ts = event.reply.thread_ts,
            text = chunks[index],
            blocks = if finished then controls(event, chunks[index]) else nil,
            mrkdwn = "false",
            parse = "none",
            unfurl_links = "false",
            unfurl_media = "false",
        }, { resource = event.reply.resource, secret = "botToken", prefix = "Bearer " }, event.reply.workspace)
        -- Wait from the response as well as from the request. This preserves
        -- a full second between provider arrivals despite different I/O delays.
        nextPost[channelKey] = os.clock() + 1
        if result.status == 429 then
            cairn.delivery.checkpoint({ status = "retryable", nextChunk = index })
            return { retryAfter = math.min(tonumber(result.headers["retry-after"]) or 5, 300) }
        end
        if result.status ~= 200 or type(result.json) ~= "table" or result.json.ok ~= true
            or result.json.channel ~= event.reply.channel or type(result.json.ts) ~= "string" then
            error("Slack did not confirm the reply. Check the thread before retrying.")
        end
        -- Advance only after a confirmed post. Each next part has its own
        -- durable write intent; a restart cannot repeat an earlier part.
        cairn.delivery.checkpoint({
            status = if finished then "sent" else "retryable",
            nextChunk = index + 1,
            ts = result.json.ts,
        })
        return if finished then { done = true } else { retryAfter = 1 }
    end,
}
