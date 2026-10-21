-- Repositories the owner imports as projects. Cairn clones the resolved remote with
-- this account's credentials and registers the project.
local github = require("./lib/github")

-- Fifty repositories stay well below the size Cairn reads at once.
local PAGE_SIZE = 50

local function badges(repo: any): { string }
    local list = {}
    table.insert(list, if repo.private then "Private" else "Public")
    if repo.fork then
        table.insert(list, "Fork")
    end
    if repo.archived then
        table.insert(list, "Archived")
    end
    if type(repo.language) == "string" and repo.language ~= "" then
        table.insert(list, repo.language)
    end
    return list
end

local function item(repo: any): any?
    local name = github.repository(repo.full_name)
    if not name then
        return nil
    end
    return {
        id = name,
        name = name,
        projectName = repo.name or name,
        description = repo.description or "",
        defaultBranch = repo.default_branch or "",
        badges = cairn.array(badges(repo)),
        updatedAt = repo.pushed_at,
        origin = "https://github.com/" .. name .. ".git",
        url = "https://github.com/" .. name,
    }
end

local function list(event: any): any
    local page = event.page or 1
    local path = string.format(
        "/user/repos?per_page=%d&page=%d&sort=pushed&direction=desc&affiliation=owner,collaborator,organization_member",
        PAGE_SIZE,
        page
    )
    local repos, failure = github.get(path)
    if not repos then
        cairn.fail(failure or "GitHub is unavailable.")
    end
    local items = {}
    for _, repo in repos do
        local entry = item(repo)
        if entry then
            table.insert(items, entry)
        end
    end
    return {
        items = cairn.array(items),
        nextPage = if #repos == PAGE_SIZE then page + 1 else nil,
    }
end

local function resolve(event: any): any
    local name = github.repository(event.id)
    if not name then
        cairn.fail("Choose a GitHub repository using owner/name.")
    end
    local repo, failure = github.get("/repos/" .. name)
    if not repo then
        cairn.fail(failure or "GitHub is unavailable.")
    end
    local resolved = github.repository(repo.full_name) or name
    return {
        id = resolved,
        name = repo.name or resolved,
        description = repo.description or "",
        defaultBranch = repo.default_branch or "",
        origin = "https://github.com/" .. resolved .. ".git",
    }
end

return {
    projects = function(event)
        if event.op == "list" then
            return list(event)
        elseif event.op == "resolve" then
            return resolve(event)
        end
        cairn.fail("Unknown project request.")
        return nil
    end,
}
