-- The Gateway session, in the instance's transport worker: identify or resume,
-- heartbeats, the threads under the channel, and admission of what is addressed to the
-- bot. Cairn keeps the socket and only what allowed people wrote.
local discord = require("./lib/discord")

local API, HEADERS = discord.API, discord.HEADERS
local snowflake, displayName = discord.snowflake, discord.displayName

-- Gateway lifecycle as Discord documents it and @discordjs/ws implements it.
local GATEWAY = "wss://gateway.discord.gg"
local QUERY = "/?v=10&encoding=json"
-- Close codes after which reconnecting cannot help: token, shard, version or intents.
local FATAL = { [4004] = true, [4010] = true, [4011] = true, [4012] = true, [4013] = true, [4014] = true }
-- Close codes after which the session is gone: reconnect and identify again.
local FRESH = { [1000] = true, [1001] = true, [4003] = true, [4005] = true, [4007] = true, [4008] = true, [4009] = true }
local HELLO_TIMEOUT = 60000
local READY_TIMEOUT = 15000
-- One Identify per bot token every five seconds (max_concurrency 1), across instances.
local IDENTIFY_PACE = 5000
-- A large guild's GUILD_CREATE can exceed what Cairn delivers whole; keep what the
-- session and thread routing need, without HTTP work on the transport worker.
local OVERSIZED = cairn.array({
    "op", "t", "s", "d.id", "d.guild_id",
    "d.threads.*.id", "d.threads.*.guild_id", "d.threads.*.parent_id", "d.threads.*.thread_metadata.archived",
})
local settings: any = {}
local gateway: any = { acked = true, threads = {} }

local function resumable(): boolean
    return gateway.session ~= nil and gateway.sequence ~= nil
end

local function forget()
    gateway.session, gateway.sequence, gateway.resume = nil, nil, nil
end

local function safeUrl(url: any): string?
    if type(url) == "string" and string.match(url, "^wss://[%w%-%.]+%.discord%.gg/?$") then
        return (string.gsub(url, "/$", ""))
    end
    return nil
end

-- Resumes go to the session's resume URL. New sessions use the URL from
-- GET /gateway/bot and wait when today's session starts are used up: past the limit,
-- Discord resets the bot's token.
local function connect()
    local wait = 0
    if resumable() then
        cairn.socket.connect({ url = (gateway.resume or GATEWAY) .. QUERY, after = wait, oversized = OVERSIZED })
        return
    end
    local url = GATEWAY
    local observed = os.clock()
    local ok, response = pcall(discord.request, "GET", "/gateway/bot")
    if ok and response.status == 401 then
        cairn.fail("Discord rejected the bot token. Reset it in the Developer Portal and save the bot again.")
    end
    local limit = if ok and response.status == 200 and type(response.json) == "table"
        then response.json.session_start_limit else nil
    if type(limit) ~= "table" or type(limit.remaining) ~= "number" or limit.remaining < 0
        or type(limit.reset_after) ~= "number" or limit.reset_after < 1 then
        cairn.instance.status("connecting", "Waiting for Discord’s session budget. Retrying shortly.")
        cairn.socket.after("connect", 5000)
        return
    end
    url = safeUrl(response.json.url) or GATEWAY
    if limit.remaining == 0 then
        wait = math.max(1, observed + math.min(limit.reset_after / 1000, 86400) - os.clock())
        cairn.instance.status(
            "connecting",
            "Discord’s daily session limit is reached. Reconnecting in about "
                .. math.max(1, math.ceil(wait / 60)) .. " min."
        )
        -- Fetch again after reset; never reuse a budget observed before the wait.
        cairn.socket.after("connect", wait * 1000)
        return
    end
    gateway.budget = { remaining = limit.remaining, expires = observed + math.min(limit.reset_after / 1000, 86400) }
    cairn.socket.connect({ url = url .. QUERY, after = wait, oversized = OVERSIZED })
end

-- Closing with 4000 keeps the session resumable; 1000 or 1001 would end it.
local function reconnect(after: number?)
    cairn.socket.close(4000)
    cairn.socket.after("connect", (after or 0) * 1000)
end

local function heartbeat()
    local sequence = if gateway.sequence then string.format("%d", gateway.sequence) else "null"
    cairn.socket.send('{"op":1,"d":' .. sequence .. "}")
    gateway.acked = false
end

-- The host replaces {{secret}} with the bot token; this script never sees it.
local function identify()
    if resumable() then
        local payload = { op = 6, d = { token = "{{secret}}", session_id = gateway.session, seq = gateway.sequence } }
        cairn.socket.send(cairn.json.encode(payload), { auth = true })
        return
    end
    -- Backoff, dialing and Hello can cross the reset boundary. Never let a stale
    -- observation lower another instance’s balance in the new window.
    local resetAfter = (gateway.budget.expires - os.clock()) * 1000
    if resetAfter < 1 then
        reconnect()
        return
    end
    -- Guilds and guild messages. Mentions carry their text without Message Content.
    local intents = if settings.messageContent then 513 + 32768 else 513
    local payload = {
        op = 2,
        d = { token = "{{secret}}", intents = intents, properties = { os = "linux", browser = "cairn", device = "cairn" } },
    }
    local result = cairn.socket.send(cairn.json.encode(payload), {
        auth = true, pace = IDENTIFY_PACE, name = "identify",
        budget = { remaining = gateway.budget.remaining, resetAfter = resetAfter },
    })
    if not result.queued then
        cairn.instance.status("connecting", "Discord’s session budget is reserved by another instance. Waiting for reset.")
        reconnect(math.max(1, result.retryAfter / 1000))
    end
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
        cairn.socket.cancel("ready")
        gateway.session, gateway.bot = data.session_id, data.user.id
        gateway.name = displayName(data.user)
        gateway.resume = safeUrl(data.resume_gateway_url)
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
        if guild ~= settings.guildId then
            return
        end
        for _, thread in data.threads or {} do
            thread.guild_id = thread.guild_id or guild
            track(thread)
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
        cairn.socket.cancel("hello")
        gateway.acked = true
        -- The first heartbeat waits a random part of the interval.
        cairn.socket.every("heartbeat", interval, interval * math.random())
        identify()
    elseif op == 11 then
        gateway.acked = true
    elseif op == 1 then
        heartbeat()
    elseif op == 7 then
        reconnect()
    elseif op == 9 then
        -- Discord asks to wait 1 to 5 seconds before identifying again.
        if payload.d ~= true then
            forget()
        end
        reconnect(1 + math.random() * 4)
    elseif op == 0 and type(payload.t) == "string" then
        dispatched(payload.t, if type(payload.d) == "table" then payload.d else {})
        -- Advance only after admission: a failed admission is replayed on resume, and
        -- replays deduplicate.
        if type(payload.s) == "number" and (gateway.sequence == nil or payload.s > gateway.sequence) then
            gateway.sequence = payload.s
        end
    end
end

return {
    socket = function(event)
        if event.type == "start" then
            settings = event.settings or {}
            -- Connect from a timer, so a slow GET /gateway/bot never delays the start.
            cairn.socket.after("connect", 0)
        elseif event.type == "open" then
            gateway.acked = true
            cairn.socket.after("hello", HELLO_TIMEOUT)
        elseif event.type == "sent" and event.name == "identify" then
            if event.sent then
                cairn.socket.after("ready", READY_TIMEOUT)
            else
                reconnect()
            end
        elseif event.type == "close" then
            if event.code and FATAL[event.code] then
                cairn.fail("Discord rejected the bot configuration. Check its token and the intents enabled in the Developer Portal.")
            end
            if event.code and FRESH[event.code] then
                forget()
            end
            cairn.instance.status("connecting")
            connect()
        elseif event.type == "tick" then
            if event.name == "connect" then
                connect()
            elseif event.name == "hello" then
                -- Connected, but Discord never said Hello.
                reconnect()
            elseif event.name == "ready" then
                -- Identified, but no READY: start over with a new session.
                forget()
                reconnect()
            elseif gateway.acked then
                heartbeat()
            else
                -- No acknowledgement since the last heartbeat: a zombie connection.
                reconnect()
            end
        elseif event.type == "message" then
            if not event.text then
                return nil
            end
            local ok, payload = pcall(cairn.json.decode, event.text)
            if ok and type(payload) == "table" then
                frame(payload)
            end
        end
        return nil
    end,
}
