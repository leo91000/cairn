-- Discord permissions: 64-bit flags, and their resolution for one member in one channel.
local ADMINISTRATOR, VIEW_CHANNEL, SEND_MESSAGES, READ_MESSAGE_HISTORY = 8, 1024, 2048, 65536
local THREADS = 2 ^ 35 + 2 ^ 38 -- create public threads, send messages in threads
local ALL = 2 ^ 53 - 1

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

return {
    VIEW_CHANNEL = VIEW_CHANNEL,
    SEND_MESSAGES = SEND_MESSAGES,
    READ_MESSAGE_HISTORY = READ_MESSAGE_HISTORY,
    THREADS = THREADS,
    band = band,
    resolve = permissions,
}
