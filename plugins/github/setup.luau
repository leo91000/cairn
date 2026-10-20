-- The instance form, test and panel: which account the token belongs to, what it may
-- do, and the commit identity runs use. They run with the token being typed, signed
-- in or saved, never seen here.
local github = require("./lib/github")

local TOKENS = "https://github.com/settings/personal-access-tokens/new"

local function truncate(value: string, limit: number): string
    if #value <= limit then
        return value
    end
    return string.sub(value, 1, limit)
end

-- The account behind the token and the scopes of an OAuth token. Fine-grained tokens
-- report no scopes: their repositories and permissions were chosen on GitHub.
local function account(): (any, string?, string?)
    local response, code = github.request("GET", "/user")
    local failure = github.problem(response, code)
    if failure then
        return nil, failure, nil
    end
    local user = response.json
    if type(user.login) ~= "string" or type(user.id) ~= "number" then
        return nil, "GitHub returned an invalid account.", nil
    end
    return user, nil, response.headers["x-oauth-scopes"]
end

-- Name and noreply address GitHub attributes commits to.
local function identity(user: any)
    local name = if type(user.name) == "string" and user.name ~= "" then user.name else user.login
    return {
        login = truncate(user.login, 100),
        gitName = truncate(name, 200),
        gitEmail = truncate(string.format("%d+%s@users.noreply.github.com", user.id, user.login), 200),
    }
end

local function hasScope(scopes: string?, wanted: string): boolean
    if not scopes then
        return false
    end
    for scope in string.gmatch(scopes, "[^,%s]+") do
        if scope == wanted then
            return true
        end
    end
    return false
end

local function scopeAlert(scopes: string?): any?
    if scopes == nil then
        return nil
    end
    if not hasScope(scopes, "workflow") then
        return {
            type = "alert",
            tone = "warning",
            title = "Workflows can't be pushed",
            text = "This token lacks the workflow scope, so agents can't change files in .github/workflows. Sign in again to grant it.",
        }
    end
    return nil
end

local function form(event: any): any
    local view = {}
    if not event.credential then
        table.insert(view, {
            type = "alert",
            tone = "info",
            title = "Connect a GitHub account",
            text = "Sign in with GitHub, or paste a token. A fine-grained token limits agents to the repositories and permissions you choose.",
        })
        table.insert(view, { type = "link", label = "Create a fine-grained token", url = TOKENS })
        return { view = cairn.array(view) }
    end
    local user, failure, scopes = account()
    if not user then
        return { errors = { credential = failure } }
    end
    local values = identity(user)
    table.insert(view, {
        type = "alert",
        tone = "success",
        title = "Signed in as " .. values.login,
        text = "Commits by agents are attributed to " .. values.gitName .. " <" .. values.gitEmail .. ">, unless this installation sets its own Git identity.",
    })
    local alert = scopeAlert(scopes)
    if alert then
        table.insert(view, alert)
    end
    return { values = values, view = cairn.array(view) }
end

return {
    form = form,

    test = function()
        local user, failure = account()
        if not user then
            cairn.fail(failure or "GitHub is unavailable.")
        end
        return { ok = true, message = "Token works · signed in as " .. user.login }
    end,

    view = function(event)
        local settings = event.instance.settings or {}
        local view = {
            {
                type = "facts",
                items = {
                    { label = "Account", value = settings.login or "Unknown until the next check" },
                    {
                        label = "Commit identity",
                        value = if settings.gitName
                            then settings.gitName .. " <" .. (settings.gitEmail or "") .. ">"
                            else "The installation's Git identity",
                    },
                },
            },
        }
        local _, _, scopes = account()
        local alert = scopeAlert(scopes)
        if alert then
            table.insert(view, alert)
        end
        return view
    end,
}
