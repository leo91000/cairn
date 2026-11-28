-- What the account's hooks designate: repositories, as owner/name, and GitHub accounts,
-- by login. Resolution asks GitHub for the exact ID; names carry no authority.
local github = require("./lib/github")

local PAGE_SIZE = 50
local MAX_PAGES = 20

local function repositories(page: number, query: string?): any
    local path = string.format(
        "/user/repos?per_page=%d&page=%d&sort=pushed&direction=desc&affiliation=owner,collaborator,organization_member",
        PAGE_SIZE,
        page
    )
    local repos, failure = github.get(path)
    if not repos then
        error(failure or "GitHub is unavailable.")
    end
    local items = {}
    for _, repo in repos do
        local name = github.repository(repo.full_name)
        if name and (not query or query == "" or string.find(string.lower(name), string.lower(query), 1, true)) then
            table.insert(items, { resourceType = "repository", id = name, name = name })
        end
    end
    return {
        items = cairn.array(items),
        nextPage = if #repos == PAGE_SIZE and page < MAX_PAGES then page + 1 else nil,
    }
end

local function people(input: any): any
    local items = {}
    local query = input.query
    if input.page == 1 then
        local own = input.instance and input.instance.settings and input.instance.settings.login
        if type(own) == "string" and own ~= "" and (not query or query == "" or string.find(string.lower(own), string.lower(query), 1, true)) then
            table.insert(items, { resourceType = "person", id = own, name = own })
        end
        if query and query ~= "" then
            local found = github.get("/search/users?per_page=30&q=" .. github.segment(query))
            for _, user in if type(found) == "table" then found.items or {} else {} do
                if type(user.login) == "string" and user.login ~= own then
                    table.insert(items, { resourceType = "person", id = user.login, name = user.login })
                end
            end
        end
    end
    return { items = cairn.array(items) }
end

return {
    resources = function(input: any): any
        if input.op == "list" then
            if input.resourceType == "repository" then
                return repositories(input.page, input.query)
            elseif input.resourceType == "person" then
                return people(input)
            end
            error("Unknown resource type")
        end
        if input.resourceType == "repository" then
            local name = github.repository(input.id)
            local repo = if name then github.get("/repos/" .. name) else nil
            if not repo or type(repo.full_name) ~= "string" or string.lower(repo.full_name) ~= string.lower(name :: string) then
                error("Resource not found")
            end
            return { resourceType = "repository", id = input.id, name = repo.full_name }
        elseif input.resourceType == "person" then
            local login = if type(input.id) == "string" and string.match(input.id, "^[%w%-]+$") and #input.id <= 100 then input.id else nil
            local user = if login then github.get("/users/" .. login) else nil
            if not user or type(user.login) ~= "string" or string.lower(user.login) ~= string.lower(login :: string) then
                error("Resource not found")
            end
            return { resourceType = "person", id = input.id, name = user.login }
        end
        error("Unknown resource type")
    end,
}
