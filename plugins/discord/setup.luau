-- The instance form, test and panel: who the bot is, where it was invited, and where it
-- can reply. They run with the token being typed or the saved one, never seen here.
local discord = require("./lib/discord")
local permissions = require("./lib/permissions")
local text = require("./lib/text")

local request, snowflake, displayName = discord.request, discord.snowflake, discord.displayName
local truncate = text.truncate
local band = permissions.band
local VIEW_CHANNEL, SEND_MESSAGES = permissions.VIEW_CHANNEL, permissions.SEND_MESSAGES
local READ_MESSAGE_HISTORY, THREADS = permissions.READ_MESSAGE_HISTORY, permissions.THREADS

-- What the bot needs: reply, react, upload deliverables and keep each conversation in
-- its own thread. The invite never asks for more.
local INVITE_PERMISSIONS = "309237746752"
local PORTAL = "https://discord.com/developers/applications"

-- A GET with the bot's token, and the sentence to show when it fails.
local function fetch(path: string): (any, string?)
    local ok, response = pcall(request, "GET", path)
    if not ok then
        return nil, "Discord is unavailable. Try again in a moment."
    end
    if response.status == 401 then
        return nil, "Discord rejected this token. Reset it in the Developer Portal and paste the new one."
    elseif response.status == 429 then
        return nil, "Discord is rate limiting this bot. Try again in a few seconds."
    elseif response.status == 403 or response.status == 404 then
        return nil, "The bot can’t see this server. Invite it, then refresh."
    elseif response.status >= 300 or response.json == nil then
        return nil, "Discord is unavailable. Try again in a moment."
    end
    return response.json, nil
end

local function channels(values: any, bot: any, options: any, derived: any, errors: any)
    local guildId = values.guildId
    local guild, problem = fetch("/guilds/" .. guildId)
    local member, memberProblem = fetch("/guilds/" .. guildId .. "/members/" .. bot.id)
    local listed, listProblem = fetch("/guilds/" .. guildId .. "/channels")
    if not guild or not member or not listed then
        errors.guildId = problem or memberProblem or listProblem
        return
    end
    -- Text and announcement channels accept the bot's replies.
    local usable = {}
    for _, channel in listed do
        if (channel.type == 0 or channel.type == 5) and snowflake(channel.id) then
            table.insert(usable, channel)
        end
    end
    table.sort(usable, function(a, b)
        return (a.position or 0) < (b.position or 0)
    end)
    local choices = {}
    local chosen = if derived.channelId == "" then nil else values.channelId
    for index, channel in usable do
        if index > 200 then
            break
        end
        local allowed = permissions.resolve(guild, member, bot.id, channel)
        local read = band(allowed, VIEW_CHANNEL) ~= 0
        local reply = read and band(allowed, SEND_MESSAGES + READ_MESSAGE_HISTORY) == SEND_MESSAGES + READ_MESSAGE_HISTORY
        local threads = read and band(allowed, THREADS) == THREADS
        table.insert(choices, {
            value = channel.id,
            label = truncate("#" .. (channel.name or channel.id), 100),
            disabled = not reply,
            description = if not read then "The bot can’t read this channel"
                elseif not reply then "The bot can’t send messages here"
                elseif not threads then "No threads · replies in the channel"
                else "Can reply",
        })
        if channel.id == chosen then
            derived.channelName = truncate(channel.name or "", 100)
            if not reply then
                errors.channelId = "The bot can’t reply in this channel. Give it access, then refresh."
            end
        end
    end
    options.channelId = cairn.array(choices)
    if chosen and not derived.channelName then
        errors.channelId = "Choose a channel of this server."
    end
end

local function form(event: any): any
    local values = event.values or {}
    local options, derived, errors, view = {}, {}, {}, {}
    -- Answering every message needs the text of messages that don't mention the bot.
    if values.everyMessage then
        derived.messageContent = true
    end
    if event.submit then
        return { values = derived }
    end
    if not event.credential then
        table.insert(view, {
            type = "alert",
            tone = "info",
            title = "Create the bot",
            text = "In the Discord Developer Portal: New Application → Bot → Reset Token, then paste the token here. To read replies without a mention, also turn on Message Content.",
        })
        table.insert(view, { type = "link", label = "Open the Developer Portal", url = PORTAL })
        return { values = derived, view = cairn.array(view) }
    end
    local bot, problem = fetch("/users/@me")
    if bot and not bot.bot then
        bot, problem = nil, "Use a bot token, not a user account token."
    end
    if not bot then
        errors.credential = problem
        return { values = derived, errors = errors }
    end
    derived.botId, derived.botName = bot.id, truncate(displayName(bot), 100)
    -- Older applications can have a client ID that differs from the bot user ID.
    local application = fetch("/oauth2/applications/@me")
    local client = if application and snowflake(application.id) then application.id else bot.id
    local guilds, guildProblem = fetch("/users/@me/guilds")
    if not guilds then
        errors.credential = guildProblem
        return { values = derived, errors = errors }
    end
    local servers, chosen = {}, nil
    for _, guild in guilds do
        if snowflake(guild.id) and #servers < 200 then
            table.insert(servers, { value = guild.id, label = truncate(guild.name or guild.id, 100) })
            if guild.id == values.guildId then
                chosen = guild
            end
        end
    end
    options.guildId = cairn.array(servers)
    table.insert(view, {
        type = "alert",
        tone = "success",
        title = "Signed in as " .. derived.botName,
        text = if #servers == 0 then "The bot isn’t in any server yet. Invite it, then refresh."
            else "Bot account · in " .. #servers .. (if #servers == 1 then " server" else " servers"),
    })
    table.insert(view, {
        type = "link",
        label = "Invite the bot to a server",
        url = "https://discord.com/oauth2/authorize?client_id=" .. client .. "&scope=bot&permissions=" .. INVITE_PERMISSIONS,
    })
    -- A new server means a new channel.
    if event.changed == "guildId" then
        derived.channelId, derived.channelName = "", ""
    end
    if values.guildId then
        if chosen then
            derived.guildName = truncate(chosen.name or "", 100)
            channels(values, bot, options, derived, errors)
        else
            errors.guildId = "The bot isn’t in this server yet. Invite it, then refresh."
        end
    end
    return { options = options, values = derived, errors = errors, view = cairn.array(view) }
end

return {
    form = form,

    test = function()
        local bot, problem = fetch("/users/@me")
        if not bot then
            cairn.fail(problem or "Discord is unavailable.")
        end
        if not bot.bot then
            cairn.fail("Use a bot token, not a user account token.")
        end
        return { ok = true, message = "Token works · signed in as " .. displayName(bot) }
    end,

    view = function(event)
        local values = event.instance.settings
        local listens = if values.everyMessage then "Every message in the channel"
            elseif values.messageContent then "Mentions, and replies in its threads"
            else "Mentions only"
        return {
            {
                type = "facts",
                items = {
                    { label = "Bot", value = values.botName or "Discord bot" },
                    { label = "Server", value = values.guildName or values.guildId },
                    { label = "Channel", value = "#" .. (values.channelName or values.channelId) },
                    { label = "Listens to", value = listens },
                },
            },
        }
    end,
}
