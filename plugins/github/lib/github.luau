-- GitHub's REST and GraphQL APIs with the instance's token, added by Cairn and never
-- seen here, and the helpers every part of the package shares.
local API = "https://api.github.com"
local HEADERS = {
    accept = "application/vnd.github+json",
    ["user-agent"] = "Cairn",
    ["x-github-api-version"] = "2022-11-28",
}

-- One call. Refused host calls raise a short code; they return nil and the code here.
local function request(method: string, path: string, body: any?, instance: string?): (any, string?)
    local ok, response = pcall(cairn.http.request, {
        method = method,
        url = API .. path,
        headers = HEADERS,
        json = body,
        auth = true,
        instance = instance,
    })
    if not ok then
        return nil, tostring(response)
    end
    return response, nil
end

-- The sentence for a response that failed, or nil.
local function problem(response: any, code: string?): string?
    if not response then
        if code and string.find(code, "invalid", 1, true) then
            return "GitHub returned more than Cairn reads at once."
        end
        return "GitHub is unavailable. Try again in a moment."
    end
    local status = response.status
    local remaining = response.headers and response.headers["x-ratelimit-remaining"]
    if status == 401 then
        return "GitHub rejected this token. Sign in again or paste a new token."
    elseif (status == 403 or status == 429) and remaining == "0" then
        return "GitHub's rate limit for this account is reached. Try again later."
    elseif status == 403 then
        return "This account can't access this on GitHub. Check its permissions."
    elseif status == 404 then
        return "GitHub could not find this repository, ref or folder."
    elseif status >= 300 or response.json == nil then
        return "GitHub is unavailable. Try again in a moment."
    end
    return nil
end

-- A GET whose JSON body is returned, with the sentence to show when it fails.
local function get(path: string, instance: string?): (any, string?, number?)
    local response, code = request("GET", path, nil, instance)
    local failure = problem(response, code)
    if failure then
        return nil, failure, response and response.status
    end
    return response.json, nil, response.status
end

-- GraphQL; any reported error makes the answer incomplete.
local function graphql(query: string, variables: any, instance: string?): any
    local response, code = request("POST", "/graphql", { query = query, variables = variables }, instance)
    local failure = problem(response, code)
    if failure then
        cairn.fail(failure)
    end
    local errors = response.json.errors
    if type(errors) == "table" and #errors > 0 then
        cairn.fail("GitHub returned an incomplete response.")
    end
    return response.json.data
end

-- `owner/name` with GitHub's characters, or nil.
local function repository(value: any): string?
    if type(value) ~= "string" then
        return nil
    end
    local owner, name = string.match(value, "^([%w%-%._]+)/([%w%-%._]+)$")
    if not owner or #owner > 100 or #name > 100 then
        return nil
    end
    for _, part in { owner, name } do
        if part == "." or part == ".." then
            return nil
        end
    end
    return value
end

-- `owner/name` of a project's remote, in every spelling Cairn stores: HTTPS, SSH URLs
-- and scp-like `git@github.com:owner/name.git`, with or without a user.
local function remote(origin: any): string?
    if type(origin) ~= "string" then
        return nil
    end
    local path = string.match(origin, "^https?://github%.com/(.+)$")
        or string.match(origin, "^ssh://github%.com/(.+)$")
        or string.match(origin, "^ssh://[%w%-%._]+@github%.com/(.+)$")
        or string.match(origin, "^git://github%.com/(.+)$")
        or string.match(origin, "^github%.com:(.+)$")
        or string.match(origin, "^[%w%-%._]+@github%.com:(.+)$")
    if not path then
        return nil
    end
    path = string.gsub(path, "/+$", "")
    path = string.gsub(path, "%.git$", "")
    return repository(path)
end

-- Percent-encodes one path segment.
local function segment(value: string): string
    return (string.gsub(value, "[^%w%-%._~]", function(character)
        return string.format("%%%02X", string.byte(character))
    end))
end

local function shortSha(value: any): string
    return if type(value) == "string" then string.sub(value, 1, 7) else ""
end

-- The instance a granted agent acts through: the first one Cairn lists.
local function first(instances: any): string?
    local instance = instances and instances[1]
    return instance and instance.id
end

return {
    API = API,
    HEADERS = HEADERS,
    request = request,
    problem = problem,
    get = get,
    graphql = graphql,
    repository = repository,
    remote = remote,
    segment = segment,
    shortSha = shortSha,
    first = first,
}
