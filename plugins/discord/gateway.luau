-- The Gateway session, in the instance's transport worker: identify or resume,
-- heartbeats, the threads under the channel, and admission of what is addressed to the
-- bot. Cairn keeps the socket and only what allowed people wrote.
local discord = require("./lib/discord")

local API, HEADERS = discord.API, discord.HEADERS
local snowflake, displayName = discord.snowflake, discord.displayName

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
}
