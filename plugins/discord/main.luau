-- The Discord bot, entirely in Luau: its Gateway connection (`socket`), its setup form
-- (`form`), its panel (`view`), its test, and what it does with each message. The
-- Gateway session admits only messages for this bot (mentions, and with Message
-- Content its threads) in the configured channel; Cairn keeps those of allowed people
-- and checks every effect.
local API = "https://discord.com/api/v10"
local HEADERS = { ["user-agent"] = "DiscordBot (https://github.com/leo91000/cairn, 1)" }
local WAITING = "Thanks! The owner of this bot needs to approve you first. Mention me again once they have."
local LIMIT = 1900
local SILENT = "NO_REPLY"
local UPLOAD_LIMIT = 10 * 1024 * 1024
local EMOJI = {
    seen = "%F0%9F%91%80", -- 👀
    done = "%E2%9C%85", -- ✅
    failed = "%E2%9D%8C", -- ❌
    stopped = "%E2%8F%B9%EF%B8%8F", -- ⏹️
}
local FAILURES = {
    rate_limited = "I’ve reached my message limit for this hour. Try again a bit later.",
    invalid = "I couldn’t read that message or one of its attachments (10 MB at most each).",
}

local function request(method: string, path: string, body: any?)
    return cairn.http.request({ method = method, url = API .. path, headers = HEADERS, json = body, auth = true })
end

local function post(channel: string, body: any)
    return request("POST", "/channels/" .. channel .. "/messages", body)
end

local function quietly(...)
    pcall(...)
end

local function react(channel: string, message: string, emoji: string, on: boolean)
    quietly(request, if on then "PUT" else "DELETE", "/channels/" .. channel .. "/messages/" .. message .. "/reactions/" .. emoji .. "/@me")
end

local function text(content: string, botId: string): string
    local value = string.gsub(content or "", "<@!?" .. botId .. ">", "")
    return string.match(value, "^%s*(.-)%s*$") or ""
end

-- Never split UTF-8 bytes; Discord validates lengths in UTF-16 code units.
local function truncate(value: string, limit: number): string
    local output, units = {}, 0
    for _, code in utf8.codes(value) do
        local size = if code > 0xFFFF then 2 else 1
        if units + size > limit then
            break
        end
        table.insert(output, utf8.char(code))
        units += size
    end
    return table.concat(output)
end

-- Discord counts message length in UTF-16 code units.
local function chunks(value: string): { string }
    local parts, current, units, count = {}, {}, 0, 0
    for _, code in utf8.codes(value) do
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

-- Thread names: the first line of the request, within Discord's 100-character limit.
local function title(value: string): string
    local line = string.match(value, "^[^\n]*") or ""
    if line == "" then
        return "Conversation"
    end
    return truncate(line, 90)
end

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


local function snowflake(value: any): boolean
    return type(value) == "string" and #value <= 20 and string.match(value, "^%d+$") ~= nil
end

local function displayName(user: any): string
    if type(user.global_name) == "string" and user.global_name ~= "" then
        return user.global_name
    end
    return user.username or ""
end

-- Gateway ---------------------------------------------------------------------------
-- The transport worker keeps one Gateway session per bot: identify or resume,
-- heartbeats, the threads under the channel, and admission of what is addressed to it.

local GATEWAY = "wss://gateway.discord.gg"
-- Close codes after which reconnecting cannot help: token, intents or shard problems.
local FATAL = { [4004] = true, [4010] = true, [4011] = true, [4012] = true, [4013] = true, [4014] = true }
local settings: any = {}
local gateway: any = { acked = true, threads = {} }

local function connect()
    cairn.socket.connect({ url = (gateway.resume or GATEWAY) .. "/?v=10&encoding=json" })
end

local function forget()
    gateway.session, gateway.sequence, gateway.resume = nil, nil, nil
end

-- Closing with 4000 keeps the session resumable.
local function reconnect()
    cairn.socket.close(4000)
    connect()
end

local function heartbeat()
    local sequence = if gateway.sequence then string.format("%d", gateway.sequence) else "null"
    cairn.socket.send('{"op":1,"d":' .. sequence .. "}")
end

-- The host replaces {{secret}} with the bot token; this script never sees it.
local function identify()
    local payload
    if gateway.session and gateway.sequence then
        payload = { op = 6, d = { token = "{{secret}}", session_id = gateway.session, seq = gateway.sequence } }
    else
        -- Guilds and guild messages. Mentions carry their text without Message Content.
        local intents = if settings.messageContent then 513 + 32768 else 513
        payload = {
            op = 2,
            d = { token = "{{secret}}", intents = intents, properties = { os = "linux", browser = "cairn", device = "cairn" } },
        }
    end
    cairn.socket.send(cairn.json.encode(payload), { auth = true })
end

local function track(thread: any)
    if not snowflake(thread.id) or thread.guild_id ~= settings.guildId then
        return
    end
    local archived = thread.thread_metadata and thread.thread_metadata.archived
    gateway.threads[thread.id] = if thread.parent_id == settings.channelId and not archived then true else nil
end

local function person(user: any): any
    if type(user) ~= "table" or not snowflake(user.id) or user.bot then
        return nil
    end
    return { id = user.id, name = displayName(user) }
end

-- The bot's scope: the configured channel and its threads. A message is for the bot when
-- it mentions it, or, with Message Content, when it is posted in one of those threads,
-- replies to the bot, or (support channels) is any new message in the channel.
local function received(message: any)
    local bot, channel = gateway.bot, message.channel_id
    local inThread = gateway.threads[channel] == true
    local mentioned = false
    for _, mention in message.mentions or {} do
        if mention.id == bot then
            mentioned = true
        end
    end
    local content = settings.messageContent == true
    local referenced = message.referenced_message
    local repliesToBot = type(referenced) == "table" and type(referenced.author) == "table" and referenced.author.id == bot
    local every = content and settings.everyMessage == true and channel == settings.channelId
    local addressed = message.guild_id == settings.guildId
        and (channel == settings.channelId or inThread)
        and message.webhook_id == nil
        and #(message.content or "") <= 50000
        and snowflake(message.id)
        and (mentioned or every or (content and (inThread or repliesToBot)))
    local author = if addressed then person(message.author) else nil
    if not author then
        return
    end
    local attachments = {}
    for index, file in message.attachments or {} do
        if index > 4 then
            break
        end
        table.insert(attachments, { url = file.url, name = file.filename, contentType = file.content_type, size = file.size })
    end
    cairn.inbound.accept({
        kind = "message",
        id = message.id,
        author = author,
        context = {
            botId = bot,
            guildId = message.guild_id,
            channelId = settings.channelId,
            threadId = if inThread then channel else nil,
            mentioned = mentioned,
            everyMessage = every,
            replyToId = message.message_reference and message.message_reference.message_id,
        },
        body = { content = message.content, attachments = cairn.array(attachments) },
    })
end

-- Button clicks must be acknowledged within three seconds: answer Discord first, then
-- queue the click like any other message.
local function clicked(interaction: any)
    local channel = interaction.channel_id
    local message = interaction.message or {}
    local inScope = interaction.guild_id == settings.guildId
        and (channel == settings.channelId or gateway.threads[channel] == true)
        and interaction.type == 3
        and type(message.author) == "table"
        and message.author.id == gateway.bot
    local id, token = interaction.id, interaction.token
    if not inScope or not snowflake(id) or type(token) ~= "string" or token == "" or #token > 500 then
        return
    end
    -- Type 6: acknowledge without changing the message; the handler updates it later.
    pcall(cairn.http.request, {
        method = "POST",
        url = API .. "/interactions/" .. id .. "/" .. token .. "/callback",
        headers = HEADERS,
        json = { type = 6 },
    })
    local author = person(interaction.member and interaction.member.user)
    local custom = interaction.data and interaction.data.custom_id
    if not author or type(custom) ~= "string" or custom == "" or #custom > 100 then
        return
    end
    cairn.inbound.accept({
        kind = "interaction",
        id = id,
        author = author,
        context = {
            botId = gateway.bot,
            guildId = interaction.guild_id,
            channelId = settings.channelId,
            threadId = if gateway.threads[channel] then channel else nil,
            messageId = message.id,
        },
        body = { customId = custom },
    })
end

local function dispatched(name: string, data: any)
    if name == "READY" then
        if type(data.user) ~= "table" or not data.user.bot then
            cairn.fail("Use a Discord bot token, not a user account token.")
        end
        gateway.session, gateway.bot = data.session_id, data.user.id
        gateway.name = displayName(data.user)
        local resume = data.resume_gateway_url
        gateway.resume = if type(resume) == "string" and string.match(resume, "^wss://[%w%-%.]+%.discord%.gg/?$")
            then (string.gsub(resume, "/$", ""))
            else nil
        cairn.instance.status("connected", "Signed in as " .. gateway.name)
    elseif name == "RESUMED" then
        cairn.instance.status("connected", if gateway.name then "Signed in as " .. gateway.name else nil)
    elseif not gateway.bot then
        return
    elseif name == "MESSAGE_CREATE" then
        received(data)
    elseif name == "INTERACTION_CREATE" then
        clicked(data)
    elseif name == "GUILD_CREATE" or name == "THREAD_LIST_SYNC" then
        local guild = data.guild_id or data.id
        if guild == settings.guildId then
            for _, thread in data.threads or {} do
                thread.guild_id = thread.guild_id or guild
                track(thread)
            end
        end
    elseif name == "THREAD_CREATE" or name == "THREAD_UPDATE" then
        track(data)
    elseif name == "THREAD_DELETE" and type(data.id) == "string" then
        gateway.threads[data.id] = nil
    end
end

local function frame(payload: any)
    local op = payload.op
    if op == 10 then
        local interval = payload.d and payload.d.heartbeat_interval
        if type(interval) ~= "number" or interval < 100 or interval > 120000 then
            reconnect()
            return
        end
        gateway.acked = true
        cairn.socket.every(interval, interval * math.random())
        identify()
    elseif op == 11 then
        gateway.acked = true
    elseif op == 1 then
        heartbeat()
    elseif op == 7 then
        reconnect()
    elseif op == 9 then
        if payload.d ~= true then
            forget()
        end
        reconnect()
    elseif op == 0 and type(payload.t) == "string" then
        dispatched(payload.t, payload.d or {})
        -- Advance only after admission: a failed admission is replayed on resume, and
        -- replays deduplicate.
        if type(payload.s) == "number" then
            gateway.sequence = payload.s
        end
    end
end

-- Setup -----------------------------------------------------------------------------
-- The instance form: who the bot is, where it was invited, and where it can reply.

-- What the bot needs: reply, react, upload deliverables and keep each conversation in
-- its own thread. The invite never asks for more.
local INVITE_PERMISSIONS = "309237746752"
local ADMINISTRATOR, VIEW_CHANNEL, SEND_MESSAGES, READ_MESSAGE_HISTORY = 8, 1024, 2048, 65536
local THREADS = 2 ^ 35 + 2 ^ 38 -- create public threads, send messages in threads
local ALL = 2 ^ 53 - 1
local PORTAL = "https://discord.com/developers/applications"

-- Discord permissions are 64-bit strings. Luau numbers are exact up to 2^53, so bit
-- operations work on 32-bit halves.
local function halves(value: number): (number, number)
    return value // 2 ^ 32, value % 2 ^ 32
end

local function both(a: number, b: number, operation: (number, number) -> number): number
    local ah, al = halves(a)
    local bh, bl = halves(b)
    return operation(ah, bh) * 2 ^ 32 + operation(al, bl)
end

local function band(a: number, b: number): number
    return both(a, b, bit32.band)
end

local function bor(a: number, b: number): number
    return both(a, b, bit32.bor)
end

local function without(a: number, b: number): number
    return both(a, b, function(x, y)
        return bit32.band(x, bit32.bnot(y))
    end)
end

local function bits(value: any): number
    local number = tonumber(value)
    if not number or number < 0 or number > ALL then
        return 0
    end
    return number
end

-- Discord's documented permission resolution for one member in one channel.
local function permissions(guild: any, member: any, user: string, channel: any): number
    if guild.owner_id == user then
        return ALL
    end
    local roles = {}
    for _, role in member.roles or {} do
        roles[role] = true
    end
    local allowed = 0
    for _, role in guild.roles or {} do
        if role.id == guild.id or roles[role.id] then
            allowed = bor(allowed, bits(role.permissions))
        end
    end
    if band(allowed, ADMINISTRATOR) ~= 0 then
        return ALL
    end
    local overwrites = channel.permission_overwrites or {}
    for _, item in overwrites do
        if item.id == guild.id then
            allowed = bor(without(allowed, bits(item.deny)), bits(item.allow))
        end
    end
    local deny, allow = 0, 0
    for _, item in overwrites do
        if item.type == 0 and roles[item.id] then
            deny, allow = bor(deny, bits(item.deny)), bor(allow, bits(item.allow))
        end
    end
    allowed = bor(without(allowed, deny), allow)
    for _, item in overwrites do
        if item.type == 1 and item.id == user then
            allowed = bor(without(allowed, bits(item.deny)), bits(item.allow))
        end
    end
    return allowed
end

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
        local allowed = permissions(guild, member, bot.id, channel)
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
    socket = function(event)
        if event.type == "start" then
            settings = event.settings or {}
            connect()
        elseif event.type == "open" then
            gateway.acked = true
        elseif event.type == "close" then
            if event.code and FATAL[event.code] then
                cairn.fail("Discord rejected the bot configuration. Check its token and the intents enabled in the Developer Portal.")
            end
            if event.code == 4007 or event.code == 4009 then
                forget()
            end
            cairn.instance.status("connecting")
            connect()
        elseif event.type == "tick" then
            if gateway.acked then
                gateway.acked = false
                heartbeat()
            else
                -- No acknowledgement since the last heartbeat: a zombie connection.
                reconnect()
            end
        elseif event.type == "message" then
            local ok, payload = pcall(cairn.json.decode, event.text)
            if ok and type(payload) == "table" then
                frame(payload)
            end
        end
        return nil
    end,

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
