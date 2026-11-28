-- Repository activity and notifications as hooks for rules. GitHub has no push channel
-- Cairn can receive, so a durable timer per watched repository polls its events and
-- finished workflow runs with conditional requests: an unchanged answer (304) costs no
-- rate limit. The first poll of a repository only sets the starting point.
local github = require("./lib/github")

local API, HEADERS = github.API, github.HEADERS

local INTERVAL = 60
local MAX_INTERVAL = 3600
local MAX_REPOSITORIES = 40
local MAX_TEXT = 4000
local MAX_SEEN = 100

local settings: any = {}
local generation = 0

-- At most `limit` bytes, never splitting a UTF-8 character.
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

local function integer(value: any): number?
    return if type(value) == "number" and value == math.floor(value) and value >= 0 then value else nil
end

local function login(value: any): string?
    return if type(value) == "table" then text(value.login, 100) else nil
end

-- Event IDs are decimal and grow; compare them without losing precision.
local function newer(id: string, than: string): boolean
    if #id ~= #than then
        return #id > #than
    end
    return id > than
end

local function emit(name: string, id: string, payload: any)
    pcall(cairn.hooks.emit, { name = name, id = id, payload = payload })
end

local function repositories(): { string }
    local list = {}
    for _, value in settings.watchRepositories or {} do
        local name = github.repository(value)
        if name and #list < MAX_REPOSITORIES then
            table.insert(list, name)
        end
    end
    return list
end

-- A GET that may answer 304. GitHub's X-Poll-Interval sets the next delay.
local function poll(path: string, validators: any): (any, number)
    local headers = table.clone(HEADERS)
    if validators.etag then
        headers["if-none-match"] = validators.etag
    elseif validators.modified then
        headers["if-modified-since"] = validators.modified
    end
    local ok, response = pcall(cairn.http.request, { method = "GET", url = API .. path, headers = headers, auth = true })
    if not ok then
        return nil, INTERVAL
    end
    local requested = tonumber(response.headers and response.headers["x-poll-interval"])
    local delay = math.clamp(requested or INTERVAL, INTERVAL, MAX_INTERVAL)
    if response.status == 304 or response.status ~= 200 or type(response.json) ~= "table" then
        if response.status == 403 or response.status == 429 then
            delay = math.max(delay, tonumber(response.headers and response.headers["retry-after"]) or 300)
        end
        return nil, math.min(delay, MAX_INTERVAL)
    end
    validators.etag = response.headers.etag
    validators.modified = response.headers["last-modified"]
    return response.json, delay
end

local function link(repository: string, path: string?): string
    return "https://github.com/" .. repository .. (path or "")
end

local function base(event: any, repository: string): any
    return { repository = repository, actorLogin = login(event.actor) or "ghost", createdAt = text(event.created_at, 40) }
end

local function with(target: any, fields: any): any
    for key, value in fields do
        target[key] = value
    end
    return target
end

local function labels(value: any): any
    local names = {}
    for _, label in value or {} do
        if type(label) == "table" and type(label.name) == "string" and #names < 50 then
            table.insert(names, text(label.name, 100))
        end
    end
    return cairn.array(names)
end

local function pull(value: any, repository: string, number: number?): any
    value = if type(value) == "table" then value else {}
    number = integer(value.number) or number
    return {
        number = number,
        title = text(value.title, 500),
        url = text(value.html_url, 2000) or (if number then link(repository, "/pull/" .. number) else nil),
        authorLogin = login(value.user),
        base = if type(value.base) == "table" then text(value.base.ref, 255) else nil,
        head = if type(value.head) == "table" then text(value.head.ref, 255) else nil,
        draft = if type(value.draft) == "boolean" then value.draft else nil,
        body = text(value.body),
    }
end

local function issue(value: any, repository: string): any
    value = if type(value) == "table" then value else {}
    local number = integer(value.number)
    return {
        number = number,
        title = text(value.title, 500),
        url = text(value.html_url, 2000) or (if number then link(repository, "/issues/" .. number) else nil),
        authorLogin = login(value.user),
        body = text(value.body),
        labels = labels(value.labels),
    }
end

-- One event of the Events API, as the hook it stands for.
local function translate(event: any, repository: string): (string?, any?)
    local kind, payload = event.type, if type(event.payload) == "table" then event.payload else {}
    local action = payload.action
    local common = base(event, repository)
    if kind == "PushEvent" then
        local ref = text(payload.ref, 255)
        if not ref then
            return nil, nil
        end
        local commits = {}
        for _, commit in payload.commits or {} do
            if type(commit) == "table" and #commits < 20 then
                table.insert(commits, {
                    sha = text(commit.sha, 40),
                    message = text(commit.message, 500),
                    authorName = if type(commit.author) == "table" then text(commit.author.name, 200) else nil,
                })
            end
        end
        local before, head = text(payload.before, 40), text(payload.head, 40)
        return "pushed", with(common, {
            ref = ref,
            branch = string.match(ref, "^refs/heads/(.+)$"),
            tag = string.match(ref, "^refs/tags/(.+)$"),
            before = before,
            head = head,
            commitCount = integer(payload.size) or (if #commits > 0 then #commits else nil),
            commits = cairn.array(commits),
            url = if before and head then link(repository, "/compare/" .. before .. "..." .. head) else link(repository),
        })
    elseif kind == "CreateEvent" or kind == "DeleteEvent" then
        local ref = text(payload.ref, 255)
        if not ref or (payload.ref_type ~= "branch" and payload.ref_type ~= "tag") then
            return nil, nil
        end
        return payload.ref_type .. (if kind == "CreateEvent" then "-created" else "-deleted"),
            with(common, { name = ref, url = link(repository) })
    elseif kind == "PullRequestEvent" then
        local names = { opened = "pull-request-opened", closed = "pull-request-closed", reopened = "pull-request-reopened" }
        if not names[action] then
            return nil, nil
        end
        local values = pull(payload.pull_request, repository, integer(payload.number))
        if not values.number then
            return nil, nil
        end
        if action == "closed" then
            values.merged = type(payload.pull_request) == "table"
                and (payload.pull_request.merged == true or type(payload.pull_request.merged_at) == "string")
        end
        return names[action], with(common, values)
    elseif kind == "PullRequestReviewEvent" then
        local review = if type(payload.review) == "table" then payload.review else {}
        local values = pull(payload.pull_request, repository, nil)
        local state = if type(review.state) == "string" then string.lower(review.state) else nil
        if not values.number or not state then
            return nil, nil
        end
        return "pull-request-reviewed", with(common, {
            number = values.number,
            title = values.title,
            state = state,
            body = text(review.body),
            url = text(review.html_url, 2000) or values.url,
        })
    elseif kind == "PullRequestReviewCommentEvent" then
        local comment = if type(payload.comment) == "table" then payload.comment else {}
        local values = pull(payload.pull_request, repository, nil)
        if action ~= "created" or not values.number or type(comment.body) ~= "string" then
            return nil, nil
        end
        return "pull-request-review-comment", with(common, {
            number = values.number,
            path = text(comment.path, 1000),
            line = integer(comment.line),
            body = text(comment.body),
            url = text(comment.html_url, 2000) or values.url,
        })
    elseif kind == "IssuesEvent" then
        local names = { opened = "issue-opened", closed = "issue-closed", reopened = "issue-reopened", labeled = "issue-labeled" }
        local values = issue(payload.issue, repository)
        if not names[action] or not values.number then
            return nil, nil
        end
        if action == "labeled" then
            values.label = if type(payload.label) == "table" then text(payload.label.name, 100) else nil
            if not values.label then
                return nil, nil
            end
        elseif action == "closed" and type(payload.issue) == "table" then
            values.reason = text(payload.issue.state_reason, 40)
        end
        return names[action], with(common, values)
    elseif kind == "IssueCommentEvent" then
        local comment = if type(payload.comment) == "table" then payload.comment else {}
        local subject = if type(payload.issue) == "table" then payload.issue else {}
        local number = integer(subject.number)
        if action ~= "created" or not number or type(comment.body) ~= "string" then
            return nil, nil
        end
        return "issue-commented", with(common, {
            number = number,
            title = text(subject.title, 500),
            isPullRequest = type(subject.pull_request) == "table",
            body = text(comment.body),
            url = text(comment.html_url, 2000),
        })
    elseif kind == "CommitCommentEvent" then
        local comment = if type(payload.comment) == "table" then payload.comment else {}
        local sha = text(comment.commit_id, 40)
        if not sha or type(comment.body) ~= "string" then
            return nil, nil
        end
        return "commit-commented", with(common, {
            sha = sha,
            path = text(comment.path, 1000),
            body = text(comment.body),
            url = text(comment.html_url, 2000),
        })
    elseif kind == "ReleaseEvent" then
        local release = if type(payload.release) == "table" then payload.release else {}
        local tag = text(release.tag_name, 255)
        if action ~= "published" or not tag then
            return nil, nil
        end
        return "release-published", with(common, {
            tag = tag,
            name = text(release.name, 500),
            prerelease = release.prerelease == true,
            body = text(release.body),
            url = text(release.html_url, 2000),
        })
    elseif kind == "WatchEvent" then
        return "starred", common
    elseif kind == "ForkEvent" then
        local fork = if type(payload.forkee) == "table" then github.repository(payload.forkee.full_name) else nil
        if not fork then
            return nil, nil
        end
        return "forked", with(common, { fork = fork, url = link(fork) })
    end
    return nil, nil
end

local function load(key: string): any
    local ok, value = pcall(cairn.storage.get, key)
    return if ok and type(value) == "table" then value else {}
end

local function save(key: string, value: any)
    pcall(cairn.storage.set, key, value)
end

local function events(repository: string, state: any): number
    local validators = { etag = state.eventsTag }
    local list, delay = poll("/repos/" .. repository .. "/events?per_page=30", validators)
    if not list then
        return delay
    end
    state.eventsTag = validators.etag
    local latest = state.lastEvent
    local fresh = {}
    for _, event in list do
        local id = if type(event) == "table" and type(event.id) == "string" and string.match(event.id, "^%d+$") then event.id else nil
        if id then
            if state.lastEvent and newer(id, state.lastEvent) then
                table.insert(fresh, 1, event)
            end
            if not latest or newer(id, latest) then
                latest = id
            end
        end
    end
    -- The first answer only marks where to start.
    state.lastEvent = latest or "0"
    for _, event in fresh do
        local name, payload = translate(event, repository)
        if name then
            emit(name, "event:" .. event.id, payload)
        end
    end
    return delay
end

local function runs(repository: string, state: any): number
    local validators = { etag = state.runsTag }
    local answer, delay = poll("/repos/" .. repository .. "/actions/runs?status=completed&per_page=20", validators)
    if not answer or type(answer.workflow_runs) ~= "table" then
        return delay
    end
    state.runsTag = validators.etag
    local first = state.runsSeen == nil
    -- Decoded JSON arrays have a protected metatable that table.clone refuses: copy.
    local seen, kept = {}, {}
    for _, key in state.runsSeen or {} do
        seen[key] = true
        table.insert(kept, key)
    end
    for index = #answer.workflow_runs, 1, -1 do
        local run = answer.workflow_runs[index]
        local id, attempt = integer(run.id), integer(run.run_attempt) or 1
        local key = if id then string.format("%d:%d", id, attempt) else nil
        if key and not seen[key] and type(run.conclusion) == "string" and type(run.head_sha) == "string" then
            seen[key] = true
            table.insert(kept, key)
            if not first then
                local numbers = {}
                for _, pr in run.pull_requests or {} do
                    if type(pr) == "table" and integer(pr.number) then
                        table.insert(numbers, pr.number)
                    end
                end
                emit("workflow-run-completed", "run:" .. key, {
                    repository = repository,
                    runId = id,
                    attempt = attempt,
                    workflow = text(run.name, 500),
                    conclusion = run.conclusion,
                    branch = text(run.head_branch, 255),
                    headSha = text(run.head_sha, 40),
                    event = text(run.event, 100),
                    actorLogin = login(run.triggering_actor) or login(run.actor),
                    pullRequests = cairn.array(numbers),
                    url = text(run.html_url, 2000),
                    completedAt = text(run.updated_at, 40),
                })
            end
        end
    end
    while #kept > MAX_SEEN do
        table.remove(kept, 1)
    end
    state.runsSeen = cairn.array(kept)
    return delay
end

-- API URLs of a notification's subject, as the page a person opens.
local function page(value: any): string?
    local path = if type(value) == "string" then string.match(value, "^https://api%.github%.com/repos/(.+)$") else nil
    if not path then
        return nil
    end
    path = string.gsub(path, "/pulls/(%d+)$", "/pull/%1")
    path = string.gsub(path, "/commits/(%x+)$", "/commit/%1")
    return text("https://github.com/" .. path, 2000)
end

local function notifications(state: any): number
    local validators = { modified = state.modified }
    local list, delay = poll("/notifications?per_page=50", validators)
    if not list then
        return delay
    end
    local first = state.started ~= true
    state.started = true
    state.modified = validators.modified or state.modified
    for index = #list, 1, -1 do
        local thread = list[index]
        local subject = if type(thread) == "table" and type(thread.subject) == "table" then thread.subject else nil
        local repository = if subject and type(thread.repository) == "table" then github.repository(thread.repository.full_name) else nil
        local updated = if subject then text(thread.updated_at, 40) else nil
        if not first and repository and updated and type(thread.id) == "string" and type(thread.reason) == "string"
            and (not state.since or updated > state.since) then
            local url = page(subject.url)
            emit("notification", "notification:" .. thread.id .. ":" .. updated, {
                threadId = text(thread.id, 100),
                reason = text(thread.reason, 100),
                title = text(subject.title, 500) or "",
                type = text(subject.type, 100) or "",
                repository = repository,
                number = if url then tonumber(string.match(url, "/(%d+)$")) else nil,
                url = url,
                updatedAt = updated,
            })
        end
        if updated and (not state.since or updated > state.since) then
            state.since = updated
        end
    end
    return delay
end

-- Each timer carries the worker's generation: one from an earlier worker stops its chain.
local function schedule(key: string, delay: number, payload: any)
    payload.generation = generation
    local ok, id = pcall(cairn.timers.after, delay, "watch", payload)
    if ok then
        local timers = load("timers")
        timers[key] = id
        save("timers", timers)
    end
end

return {
    start = function(event: any)
        settings = event.settings or {}
        local previous = load("watch")
        generation = (integer(previous.generation) or 0) + 1
        save("watch", { generation = generation })
        for _, id in load("timers") do
            pcall(cairn.timers.cancel, id)
        end
        save("timers", {})
        -- Spread the first polls over the first minute.
        for index, repository in repositories() do
            schedule("repository:" .. repository, 1 + (index - 1) % INTERVAL, { repository = repository })
        end
        if settings.watchNotifications == true then
            schedule("notifications", 1, { notifications = true })
        end
        return nil
    end,

    timer = function(event: any)
        local payload = event.payload or {}
        if event.name ~= "watch" or payload.generation ~= generation then
            return nil
        end
        if payload.notifications then
            if settings.watchNotifications ~= true then
                return nil
            end
            local state = load("notifications")
            local delay = notifications(state)
            save("notifications", state)
            schedule("notifications", delay, { notifications = true })
            return nil
        end
        local repository = github.repository(payload.repository)
        if not repository or not table.find(repositories(), repository) then
            return nil
        end
        local key = "repository:" .. repository
        local state = load(key)
        local delay = math.max(events(repository, state), runs(repository, state))
        save(key, state)
        schedule(key, delay, { repository = repository })
        return nil
    end,
}
