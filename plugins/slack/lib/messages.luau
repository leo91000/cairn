-- All message writers in an app's event worker share Slack's channel pace.
-- Prime replacement workers too, so a restart cannot burst after its predecessor.
local startedAfter = os.clock() + 1
local nextPost = {}

local function key(reply)
    return reply.workspace .. ":" .. reply.channel
end

local function reserve(reply)
    nextPost[key(reply)] = os.clock() + 1
end

return {
    ready = function(reply)
        local now = os.clock()
        local remaining = math.max(startedAfter, nextPost[key(reply)] or 0) - now
        for channel, deadline in nextPost do
            if deadline <= now then
                nextPost[channel] = nil
            end
        end
        return if remaining > 0 then remaining else nil
    end,
    before = reserve,
    after = reserve,
}
