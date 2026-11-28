-- What happens in the bot's server, as hooks for rules: messages, edits, reactions,
-- button presses, threads, channels and members. Cairn records only the hooks a rule
-- listens to; the Gateway session keeps running whatever an emission answers.
local discord = require("./lib/discord")

local snowflake, displayName = discord.snowflake, discord.displayName

local KINDS = {
    [0] = "text",
    [2] = "voice",
    [4] = "category",
    [5] = "announcement",
    [13] = "stage",
    [15] = "forum",
    [16] = "media",
}
local THREAD_TYPES = { [10] = true, [11] = true, [12] = true }
local MAX_TEXT = 4000

local events = {}

-- An emission that fails (an invalid field from Discord) never stops the session.
local function emit(name: string, id: string, payload: any)
    pcall(cairn.hooks.emit, { name = name, id = id, payload = payload })
end

local function text(value: any, limit: number?): string?
    if type(value) ~= "string" then
        return nil
    end
    local max = limit or MAX_TEXT
    if #value <= max then
        return value
    end
    local cut = utf8.offset(value, 0, max + 1) or (max + 1)
    return string.sub(value, 1, cut - 1)
end

-- A message's channel and thread: a thread's messages name the thread as their channel.
local function place(state: any, channel: string): (string, string?)
    local parent = state.parents[channel]
    if parent then
        return parent, channel
    end
    return channel, nil
end

local function link(guild: string, channel: string, message: string): string
    return "https://discord.com/channels/" .. guild .. "/" .. channel .. "/" .. message
end

local function emoji(value: any): (string?, string?)
    if type(value) ~= "table" then
        return nil, nil
    end
    local name = text(value.name, 100)
    if snowflake(value.id) then
        return (name or "emoji") .. ":" .. value.id, name
    end
    return name, name
end

local function user(value: any): any?
    if type(value) ~= "table" or not snowflake(value.id) then
        return nil
    end
    return { id = value.id, name = text(displayName(value), 100) or value.id, bot = value.bot == true }
end

-- Threads of the server and their parent channel, so messages name both.
function events.track(state: any, thread: any)
    if type(thread) ~= "table" or not snowflake(thread.id) or not snowflake(thread.parent_id) then
        return
    end
    state.parents[thread.id] = thread.parent_id
end

local function message(state: any, data: any)
    local author = user(data.author)
    if not author or author.id == state.bot or not snowflake(data.id) or not snowflake(data.channel_id) then
        return
    end
    local mentioned = false
    for _, mention in data.mentions or {} do
        if type(mention) == "table" and mention.id == state.bot then
            mentioned = true
        end
    end
    local attachments = {}
    for index, file in data.attachments or {} do
        if index > 4 then
            break
        end
        table.insert(attachments, {
            name = text(file.filename, 300),
            url = text(file.url, 2000),
            contentType = text(file.content_type, 200),
            size = if type(file.size) == "number" then file.size else nil,
        })
    end
    local channel, thread = place(state, data.channel_id)
    local reply = data.message_reference and data.message_reference.message_id
    emit("message", "message:" .. data.id, {
        messageId = data.id,
        guildId = data.guild_id,
        channelId = channel,
        threadId = thread,
        authorId = author.id,
        authorName = author.name,
        authorBot = author.bot or data.webhook_id ~= nil,
        text = text(data.content) or "",
        mentionsBot = mentioned,
        replyToId = if snowflake(reply) then reply else nil,
        url = link(data.guild_id, data.channel_id, data.id),
        createdAt = text(data.timestamp, 40),
        attachments = cairn.array(attachments),
    })
end

-- Link previews also update messages; only edits by their author are hooks.
local function edited(state: any, data: any)
    local at = text(data.edited_timestamp, 40)
    if not at or not snowflake(data.id) or not snowflake(data.channel_id) then
        return
    end
    local author = user(data.author)
    if author and author.id == state.bot then
        return
    end
    local channel, thread = place(state, data.channel_id)
    emit("message-edited", "edit:" .. data.id .. ":" .. at, {
        messageId = data.id,
        guildId = data.guild_id,
        channelId = channel,
        threadId = thread,
        authorId = author and author.id,
        authorName = author and author.name,
        text = text(data.content),
        editedAt = at,
        url = link(data.guild_id, data.channel_id, data.id),
    })
end

local function deleted(state: any, data: any)
    if not snowflake(data.id) or not snowflake(data.channel_id) then
        return
    end
    local channel, thread = place(state, data.channel_id)
    emit("message-deleted", "delete:" .. data.id, {
        messageId = data.id,
        guildId = data.guild_id,
        channelId = channel,
        threadId = thread,
    })
end

-- Reactions and members have no ID of their own: the session and sequence of the
-- dispatch identify it, and a resume replays both unchanged.
local function reaction(state: any, name: string, data: any, key: string)
    local value, label = emoji(data.emoji)
    if not value or not snowflake(data.user_id) or data.user_id == state.bot
        or not snowflake(data.message_id) or not snowflake(data.channel_id) then
        return
    end
    local member = data.member and user(data.member.user)
    local channel, thread = place(state, data.channel_id)
    emit(name, key, {
        messageId = data.message_id,
        guildId = data.guild_id,
        channelId = channel,
        threadId = thread,
        userId = data.user_id,
        userName = if member then member.name else nil,
        emoji = value,
        emojiName = label,
        messageAuthorId = if snowflake(data.message_author_id) then data.message_author_id else nil,
    })
end

local function thread(state: any, data: any)
    events.track(state, data)
    if data.newly_created ~= true or not snowflake(data.id) or not snowflake(data.parent_id) then
        return
    end
    emit("thread-created", "thread:" .. data.id, {
        threadId = data.id,
        guildId = data.guild_id,
        channelId = data.parent_id,
        name = text(data.name, 100) or "",
        ownerId = if snowflake(data.owner_id) then data.owner_id else nil,
    })
end

local function threadDeleted(state: any, data: any)
    local parent = data.parent_id or state.parents[data.id]
    state.parents[data.id] = nil
    if not snowflake(data.id) or not snowflake(parent) then
        return
    end
    emit("thread-deleted", "threadDeleted:" .. data.id, {
        threadId = data.id,
        guildId = data.guild_id,
        channelId = parent,
    })
end

local function channel(name: string, data: any)
    if not snowflake(data.id) or THREAD_TYPES[data.type] then
        return
    end
    if name == "CHANNEL_CREATE" then
        emit("channel-created", "channel:" .. data.id, {
            channelId = data.id,
            guildId = data.guild_id,
            name = text(data.name, 100) or "",
            kind = KINDS[data.type] or "other",
            categoryId = if snowflake(data.parent_id) then data.parent_id else nil,
        })
    else
        emit("channel-deleted", "channelDeleted:" .. data.id, {
            channelId = data.id,
            guildId = data.guild_id,
            name = text(data.name, 100) or "",
        })
    end
end

local function member(name: string, data: any, key: string)
    local person = user(data.user)
    if not person then
        return
    end
    local nick = text(data.nick, 100)
    emit(if name == "GUILD_MEMBER_ADD" then "member-joined" else "member-left", key, {
        userId = person.id,
        userName = if nick and nick ~= "" then nick else person.name,
        guildId = data.guild_id,
        bot = person.bot,
        joinedAt = if name == "GUILD_MEMBER_ADD" then text(data.joined_at, 40) else nil,
    })
end

-- A button of one of the bot's messages, already acknowledged by the transport.
function events.pressed(state: any, interaction: any, person: any)
    local message = interaction.message or {}
    local custom = interaction.data and interaction.data.custom_id
    if not person or type(custom) ~= "string" or custom == "" or #custom > 100
        or not snowflake(message.id) or not snowflake(interaction.channel_id) then
        return
    end
    local channel, thread = place(state, interaction.channel_id)
    emit("button-pressed", "button:" .. interaction.id, {
        interactionId = interaction.id,
        customId = custom,
        messageId = message.id,
        guildId = interaction.guild_id,
        channelId = channel,
        threadId = thread,
        userId = person.id,
        userName = person.name,
    })
end

-- One Gateway dispatch of the configured server. `sequence` is its position in the session.
function events.dispatch(state: any, name: string, data: any, sequence: number?)
    if data.guild_id ~= state.guildId then
        return
    end
    local key = if state.session and sequence then state.session .. ":" .. string.format("%d", sequence) else nil
    if name == "MESSAGE_CREATE" then
        message(state, data)
    elseif name == "MESSAGE_UPDATE" then
        edited(state, data)
    elseif name == "MESSAGE_DELETE" then
        deleted(state, data)
    elseif (name == "MESSAGE_REACTION_ADD" or name == "MESSAGE_REACTION_REMOVE") and key then
        reaction(state, if name == "MESSAGE_REACTION_ADD" then "reaction-added" else "reaction-removed", data, "reaction:" .. key)
    elseif name == "THREAD_CREATE" or name == "THREAD_UPDATE" then
        thread(state, data)
    elseif name == "THREAD_DELETE" then
        threadDeleted(state, data)
    elseif name == "CHANNEL_CREATE" or name == "CHANNEL_DELETE" then
        channel(name, data)
    elseif (name == "GUILD_MEMBER_ADD" or name == "GUILD_MEMBER_REMOVE") and key then
        member(name, data, "member:" .. key)
    end
end

return events
