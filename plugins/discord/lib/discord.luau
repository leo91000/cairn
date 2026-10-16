-- Discord's REST API with the bot's token (added by Cairn, never seen here), and the
-- identities and reactions every part of the bot uses.
local API = "https://discord.com/api/v10"
local HEADERS = { ["user-agent"] = "DiscordBot (https://github.com/leo91000/cairn, 1)" }
local EMOJI = {
    seen = "%F0%9F%91%80", -- 👀
    done = "%E2%9C%85", -- ✅
    failed = "%E2%9D%8C", -- ❌
    stopped = "%E2%8F%B9%EF%B8%8F", -- ⏹️
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


local function snowflake(value: any): boolean
    return type(value) == "string" and #value <= 20 and string.match(value, "^%d+$") ~= nil
end

local function displayName(user: any): string
    if type(user.global_name) == "string" and user.global_name ~= "" then
        return user.global_name
    end
    return user.username or ""
end

return {
    API = API,
    HEADERS = HEADERS,
    EMOJI = EMOJI,
    request = request,
    post = post,
    quietly = quietly,
    react = react,
    snowflake = snowflake,
    displayName = displayName,
}
