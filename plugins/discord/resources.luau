-- What the bot's hooks designate, for pickers and grants restricted to a channel: its
-- server, the server's channels and threads, and its members. Resolution checks every
-- ID against Discord itself and the configured server; names carry no authority.
local discord = require("./lib/discord")

local request, snowflake, displayName = discord.request, discord.snowflake, discord.displayName

local THREAD_TYPES = { [10] = true, [11] = true, [12] = true }
-- Channels that hold messages or threads; categories only group them.
local CHANNEL_TYPES = { [0] = true, [2] = true, [5] = true, [13] = true, [15] = true, [16] = true }
local LIMIT = 200

local function fetch(path: string): any
    local ok, response = pcall(request, "GET", path)
    if not ok then
        error("Discord is unavailable. Try again in a moment.")
    end
    if response.status == 403 then
        error("The bot can't see this on Discord.")
    elseif response.status == 404 then
        error("Resource not found")
    elseif response.status >= 300 or response.json == nil then
        error("Discord is unavailable. Try again in a moment.")
    end
    return response.json
end

local function name(value: any, fallback: string): string
    if type(value) ~= "string" or value == "" then
        return fallback
    end
    if #value <= 300 then
        return value
    end
    return string.sub(value, 1, (utf8.offset(value, 0, 301) or 301) - 1)
end

local function matches(label: string, query: string?): boolean
    return query == nil or query == "" or string.find(string.lower(label), string.lower(query), 1, true) ~= nil
end

local function server(guild: string): any
    return { resourceType = "server", id = guild }
end

local function channelOf(guild: string, channel: any): any
    return {
        resourceType = "channel",
        id = channel.id,
        name = name("#" .. (channel.name or ""), channel.id),
        parent = server(guild),
    }
end

local function threadOf(thread: any): any
    return {
        resourceType = "thread",
        id = thread.id,
        name = name(thread.name, thread.id),
        parent = { resourceType = "channel", id = thread.parent_id },
    }
end

local function personOf(member: any): any?
    local user = member.user
    if type(user) ~= "table" or not snowflake(user.id) then
        return nil
    end
    local label = if type(member.nick) == "string" and member.nick ~= "" then member.nick else displayName(user)
    return { resourceType = "person", id = user.id, name = name(label, user.id) }
end

local function list(guild: string, input: any): any
    local items = {}
    local query = input.query
    if input.page > 1 then
        return { items = cairn.array(items) }
    end
    if input.resourceType == "server" then
        local value = fetch("/guilds/" .. guild)
        table.insert(items, { resourceType = "server", id = guild, name = name(value.name, guild) })
    elseif input.resourceType == "channel" then
        for _, channel in fetch("/guilds/" .. guild .. "/channels") do
            local item = if CHANNEL_TYPES[channel.type] and snowflake(channel.id) then channelOf(guild, channel) else nil
            if item and matches(item.name, query) and #items < LIMIT then
                table.insert(items, item)
            end
        end
    elseif input.resourceType == "thread" then
        local active = fetch("/guilds/" .. guild .. "/threads/active")
        for _, thread in active.threads or {} do
            local item = if snowflake(thread.id) and snowflake(thread.parent_id) then threadOf(thread) else nil
            if item and matches(item.name, query) and #items < LIMIT then
                table.insert(items, item)
            end
        end
    elseif input.resourceType == "person" then
        -- Listing members needs Server Members Intent; a search does too.
        local path = if query and query ~= ""
            then "/guilds/" .. guild .. "/members/search?limit=100&query=" .. (string.gsub(query, "[^%w%-%._~]", function(c)
                return string.format("%%%02X", string.byte(c))
            end))
            else "/guilds/" .. guild .. "/members?limit=" .. LIMIT
        local ok, members = pcall(fetch, path)
        if not ok then
            error("Discord did not list the server's members. Turn on Server Members Intent for the bot in the Developer Portal.")
        end
        for _, member in members do
            local item = personOf(member)
            if item and #items < LIMIT then
                table.insert(items, item)
            end
        end
    else
        error("Unknown resource type")
    end
    return { items = cairn.array(items) }
end

local function resolve(guild: string, input: any): any
    local id = input.id
    if not snowflake(id) then
        error("Resource not found")
    end
    if input.resourceType == "server" then
        if id ~= guild then
            error("Resource not found")
        end
        local value = fetch("/guilds/" .. guild)
        return { resourceType = "server", id = guild, name = name(value.name, guild) }
    elseif input.resourceType == "channel" or input.resourceType == "thread" then
        local channel = fetch("/channels/" .. id)
        local thread = THREAD_TYPES[channel.type] == true
        local kind = thread or CHANNEL_TYPES[channel.type] == true
        if channel.id ~= id or channel.guild_id ~= guild or not kind or thread ~= (input.resourceType == "thread") then
            error("Resource not found")
        end
        if thread then
            if not snowflake(channel.parent_id) then
                error("Resource not found")
            end
            return threadOf(channel)
        end
        return channelOf(guild, channel)
    elseif input.resourceType == "person" then
        local item = personOf(fetch("/guilds/" .. guild .. "/members/" .. id))
        if not item or item.id ~= id then
            error("Resource not found")
        end
        return item
    end
    error("Unknown resource type")
end

return {
    resources = function(input: any): any
        local guild = input.instance and input.instance.settings and input.instance.settings.guildId
        if not snowflake(guild) then
            error("Choose the bot's server first.")
        end
        if input.op == "list" then
            return list(guild, input)
        end
        return resolve(guild, input)
    end,
}
