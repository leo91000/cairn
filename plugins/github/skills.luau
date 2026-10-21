-- Skills imported from a repository folder at a commit. Cairn downloads the listed
-- blobs with this account's token, picks the skill folder and compares commits.
local github = require("./lib/github")

-- Cairn reads at most this many files of a folder.
local MAX_FILES = 2000

local function reference(value: any): string?
    if type(value) ~= "string" or value == "" or #value > 200 then
        return nil
    end
    if string.find(value, "..", 1, true) or string.sub(value, 1, 1) == "/" or string.sub(value, -1) == "/" then
        return nil
    end
    if not string.match(value, "^[%w%-%._/]+$") then
        return nil
    end
    return value
end

local function sha(value: any): string?
    if type(value) == "string" and #value == 40 and string.match(value, "^%x+$") then
        return value
    end
    return nil
end

-- `owner/name`, or a GitHub address with an optional `/tree/<ref>/<folder>`.
local function parse(raw: string): (string?, string?, string?)
    local value = string.gsub(string.gsub(raw, "^%s+", ""), "%s+$", "")
    local path = string.match(value, "^https://github%.com/(.+)$") or string.match(value, "^github%.com/(.+)$")
    if not path then
        return github.repository(value), nil, nil
    end
    path = string.gsub(path, "%.git$", "")
    local owner, name, rest = string.match(path, "^([^/]+)/([^/]+)(.*)$")
    if not owner then
        return nil, nil, nil
    end
    local repository = github.repository(owner .. "/" .. name)
    local ref, folder = string.match(rest, "^/tree/([^/]+)/?(.*)$")
    if not ref then
        ref, folder = string.match(rest, "^/blob/([^/]+)/?(.*)$")
        -- A link to SKILL.md names its folder.
        if folder then
            folder = string.match(folder, "^(.*)/[^/]*$") or ""
        end
    end
    return repository, ref, folder
end

local function commit(repository: string, ref: string): any
    local value, failure = github.get("/repos/" .. repository .. "/commits/" .. github.segment(ref))
    if not value then
        cairn.fail(failure or "GitHub is unavailable.")
    end
    local tree = value.commit and value.commit.tree and value.commit.tree.sha
    if not sha(value.sha) or not sha(tree) then
        cairn.fail("Invalid GitHub response.")
    end
    local message = value.commit.message or ""
    return {
        sha = value.sha,
        tree = tree,
        message = string.match(message, "^[^\n]*") or "",
        date = value.commit.committer and value.commit.committer.date or "",
    }
end

local function resolve(event: any): any
    local location = event.location or {}
    local repository, linkedRef, folder = parse(location.repository or "")
    if not repository then
        cairn.fail("Choose a GitHub repository using owner/name or its address.")
    end
    local ref = location.ref
    if ref == nil or ref == "" then
        ref = linkedRef
    end
    if ref == nil or ref == "" then
        local details, failure = github.get("/repos/" .. repository)
        if not details then
            cairn.fail(failure or "GitHub is unavailable.")
        end
        ref = details.default_branch
    end
    if not reference(ref) then
        cairn.fail("Choose a branch, tag or commit.")
    end
    local latest = commit(repository, ref)
    return {
        location = { repository = repository, ref = ref },
        version = latest.sha,
        folder = folder or "",
        label = repository,
        url = "https://github.com/" .. repository .. "/tree/" .. ref,
        message = latest.message,
        date = latest.date,
    }
end

local function files(event: any): any
    local location = event.location or {}
    local repository = github.repository(location.repository)
    local version = sha(event.version)
    if not repository or not version then
        cairn.fail("Invalid skill source.")
    end
    local folder = event.folder or ""
    local tree = commit(repository, version).tree
    for part in string.gmatch(folder, "[^/]+") do
        local listing, failure = github.get("/repos/" .. repository .. "/git/trees/" .. tree)
        if not listing then
            cairn.fail(failure or "GitHub is unavailable.")
        end
        local found
        for _, entry in listing.tree or {} do
            if entry.path == part and entry.type == "tree" then
                found = entry.sha
                break
            end
        end
        if not found then
            cairn.fail(folder .. " is not a folder in " .. repository .. ".")
        end
        tree = found
    end
    -- A recursive listing larger than Cairn reads at once is partial.
    local listing = github.get("/repos/" .. repository .. "/git/trees/" .. tree .. "?recursive=1")
    if not listing then
        return { files = cairn.array(), truncated = true }
    end
    local result = {}
    local truncated = listing.truncated == true
    for _, entry in listing.tree or {} do
        if entry.type == "blob" and type(entry.path) == "string" and sha(entry.sha) then
            if #result >= MAX_FILES then
                truncated = true
                break
            end
            table.insert(result, {
                path = entry.path,
                size = entry.size or 0,
                digest = entry.sha,
                executable = entry.mode == "100755",
                link = entry.mode == "120000",
                download = {
                    url = github.API .. "/repos/" .. repository .. "/git/blobs/" .. entry.sha,
                    headers = {
                        accept = "application/vnd.github.raw+json",
                        ["user-agent"] = "Cairn",
                        ["x-github-api-version"] = "2022-11-28",
                    },
                },
            })
        end
    end
    return { files = cairn.array(result), truncated = truncated }
end

return {
    skills = function(event)
        if event.op == "resolve" then
            return resolve(event)
        elseif event.op == "files" then
            return files(event)
        end
        cairn.fail("Unknown skill source request.")
        return nil
    end,
}
