local API = "https://slack.com/api/"

local RETRYABLE_READ_ERRORS = {
    fatal_error = true,
    internal_error = true,
    service_unavailable = true,
    org_login_required = true,
    team_added_to_org = true,
}

local function classifyResponse(method, response)
    if response.status ~= 200 or type(response.json) ~= "table" or response.json.ok ~= false then
        return response
    end
    local code = response.json.error
    local throttled = code == "ratelimited" or code == "rate_limited"
    local status = if throttled then 429 else if method == "GET" and RETRYABLE_READ_ERRORS[code] then 503 else nil
    if not status then
        return response
    end
    local classified = table.clone(response)
    classified.status = status
    return classified
end

local function reserve(workspace, method)
    local key = "cooldown:" .. workspace .. ":" .. method
    local deadline = cairn.storage.get(key)
    if deadline == nil then
        -- Reserve before provider I/O, never after a definite 429. If the
        -- bounded general store is full, refuse before importing any work.
        deadline = cairn.storage.max(key, 0)
    end
    assert(type(deadline) == "number", "The Slack cooldown is not a number")
    return key, deadline
end

local function request(method, name, values, auth, workspace)
    local url = API .. name
    if method == "GET" and values then
        local query = {}
        local function encode(value)
            return (string.gsub(value, "[^%w%-%._~]", function(character)
                return string.format("%%%02X", string.byte(character))
            end))
        end
        for key, value in values do
            table.insert(query, encode(key) .. "=" .. encode(value))
        end
        url ..= "?" .. table.concat(query, "&")
    end
    -- One app instance may have several channel resources in the same verified
    -- workspace. Slack's method limit belongs to that workspace, not a resource.
    if auth and auth.resource then
        assert(type(workspace) == "string" and workspace ~= "", "Name the verified Slack workspace")
    end
    local key, retryAt = nil, nil
    if auth and auth.resource then
        key, retryAt = reserve(workspace, name)
    end
    if type(retryAt) == "number" and retryAt > os.time() then
        return { status = 429, headers = { ["retry-after"] = tostring(retryAt - os.time()) } }
    end
    local response = cairn.http.request({
        method = method,
        url = url,
        form = if method == "GET" then nil else values,
        auth = auth,
    })
    -- A documented JSON rate rejection is definite, even for a write. Other
    -- transient errors are retryable only for reads: a mutation may have committed.
    response = classifyResponse(method, response)
    if key and response.status == 429 then
        local seconds = tonumber(response.headers["retry-after"])
        if not seconds or seconds < 0 or seconds == math.huge or seconds ~= seconds then
            error("Slack returned an invalid Retry-After; review the connection before retrying.")
        end
        -- os.time is integral: one extra second prevents rounding the wait down.
        cairn.storage.max(key, os.time() + math.max(1, math.ceil(seconds)) + 1)
        -- Retain expired reservations. Success cannot erase a newer worker's
        -- deadline, and subsequent 429s reuse a slot even at storage capacity.
    end
    return response
end

return {
    request = request,
    channel = function(resource, channel, workspace)
        -- Every accepted mention may need this method later. Reserve its quota
        -- slot before metadata I/O/admission, even if the person is then denied.
        reserve(workspace, "chat.postMessage")
        local result = request("GET", "conversations.info", { channel = channel },
            { resource = resource, secret = "botToken", prefix = "Bearer " }, workspace)
        local info = if result.status == 200 and type(result.json) == "table" and result.json.ok == true then result.json.channel else nil
        local safe = type(info) == "table" and info.id == channel and info.is_private == false
            and info.is_archived == false and info.is_member == true and info.is_shared == false
            and info.is_ext_shared ~= true and info.is_org_shared ~= true and info.is_pending_ext_shared ~= true
        return safe, result
    end,
    configuration = function(name, values)
        return request(if name == "auth.test" then "POST" else "GET", name, values, { configuration = "self", secret = "botToken", prefix = "Bearer " })
    end,
    problem = function(response)
        if response.status == 429 then
            return "Slack is rate limiting configuration. Wait before trying again."
        end
        if response.status ~= 200 or type(response.json) ~= "table" then
            return "Slack is unavailable. Try again before saving."
        end
        if response.json.ok ~= true then
            -- Never echo arbitrary provider content or credentials into a form.
            return "Slack refused this check. Verify the bot token, scopes and channel membership."
        end
        return nil
    end,
}
