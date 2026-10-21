-- What agents wait for with wait_for: a workflow run, or the required checks of a pull
-- request, pinned to a commit. Returning nil means not complete yet; an error is
-- retried until the deadline; { state = "unavailable", message } ends the wait.
local github = require("./lib/github")

local shortSha = github.shortSha

-- Both queries bind the repository and PR explicitly. No branch/current-directory inference.
local IDENTITY = [[
query($owner:String!,$name:String!,$number:Int!) {
  repository(owner:$owner,name:$name) {
    pullRequest(number:$number) {
      number headRefOid state baseRefName
      baseRef { branchProtectionRule {
        requiresStatusChecks requiredStatusChecks { context app { databaseId } }
      } }
    }
  }
}]]

local CHECKS = [[
query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
  repository(owner:$owner,name:$name) {
    pullRequest(number:$number) {
      number headRefOid state baseRefName
      baseRef { branchProtectionRule {
        requiresStatusChecks requiredStatusChecks { context app { databaseId } }
      } }
      commits(last:1) { nodes { commit {
        oid statusCheckRollup { contexts(first:100,after:$cursor) {
          totalCount pageInfo { hasNextPage endCursor }
          nodes {
            __typename
            ...on CheckRun {
              name status conclusion isRequired(pullRequestNumber:$number)
              checkSuite { commit { oid } app { databaseId } }
            }
            ...on StatusContext {
              context state isRequired(pullRequestNumber:$number)
            }
          }
        } }
      } } }
    }
  }
}]]

local MAX_CHECK_PAGES = 5
local MAX_RULE_PAGES = 2
local MAX_REQUIRED = 500

local WORKFLOW_PENDING = { queued = true, in_progress = true, requested = true, waiting = true, pending = true }
local WORKFLOW_CONCLUSIONS = {
    success = true,
    failure = true,
    neutral = true,
    cancelled = true,
    skipped = true,
    timed_out = true,
    action_required = true,
    stale = true,
    startup_failure = true,
}

-- A limit or an unsupported setup: the wait ends with this message.
local Unavailable = {}
Unavailable.__index = Unavailable

local function unavailable(message: string)
    error(setmetatable({ message = message }, Unavailable), 0)
end

local function invalid()
    cairn.fail("Invalid GitHub PR checks response.")
end

local function lower(value: any): string
    return if type(value) == "string" then string.lower(value) else ""
end

local function words(value: string): string
    return (string.gsub(value, "_", " "))
end

-- The project's repository, from its remote.
local function repository(event: any): string
    local name = github.remote(event.project and event.project.origin)
    if not name then
        unavailable("This condition requires a github.com project.")
    end
    return name :: string
end

local function workflow(event: any, instance: string?): any?
    local input = event.input
    local name = repository(event)
    local run, failure, status = github.get("/repos/" .. name .. "/actions/runs/" .. string.format("%d", input.runId), instance)
    if not run then
        -- A 404 also hides a private repository from an account without access: retry.
        if status == 404 then
            cairn.fail("GitHub could not find this workflow run. Check that the account can read the repository.")
        end
        cairn.fail(failure or "GitHub could not be checked. Verify the connection and retry.")
    end
    if run.id ~= input.runId or lower(run.head_sha) ~= lower(input.headSha) then
        unavailable("The workflow run does not match the requested run ID and commit SHA.")
    end
    if run.status ~= "completed" then
        if not WORKFLOW_PENDING[run.status] then
            cairn.fail("Unknown GitHub workflow state.")
        end
        return nil
    end
    local conclusion = run.conclusion
    if not WORKFLOW_CONCLUSIONS[conclusion] then
        cairn.fail("Unknown GitHub workflow conclusion.")
    end
    local attempt = if type(run.run_attempt) == "number" then run.run_attempt else nil
    local detail = if attempt and attempt > 1 then "Attempt " .. attempt else ""
    local display
    if conclusion == "success" then
        display = { tone = "ok", title = "Workflow passed", detail = detail }
    elseif conclusion == "failure" or conclusion == "startup_failure" or conclusion == "timed_out" then
        local how = if conclusion == "failure" then "" else words(conclusion)
        local parts = {}
        for _, part in { how, detail } do
            if part ~= "" then
                table.insert(parts, part)
            end
        end
        display = { tone = "error", title = "Workflow failed", detail = table.concat(parts, " · ") }
    else
        display = { tone = "warning", title = "Workflow " .. words(conclusion), detail = detail }
    end
    return {
        state = "completed",
        repository = name,
        runId = run.id,
        headSha = lower(input.headSha),
        status = run.status,
        conclusion = conclusion,
        runAttempt = run.run_attempt,
        url = run.html_url,
        display = display,
    }
end

local function query(name: string, input: any, text: string, cursor: string?, instance: string?): any
    local owner, repo = string.match(name, "^([^/]+)/(.+)$")
    local data = github.graphql(text, {
        owner = owner,
        name = repo,
        number = input.prNumber,
        cursor = cursor,
    }, instance)
    local pr = data and data.repository and data.repository.pullRequest
    if type(pr) ~= "table" then
        invalid()
    end
    return pr
end

-- Superseded or closed, or nil while the PR still has the requested open head.
local function identity(pr: any, name: string, input: any): any?
    local head = pr.headRefOid
    if pr.number ~= input.prNumber
        or type(head) ~= "string"
        or not string.match(head, "^%x+$")
        or #head ~= 40
        or not (pr.state == "OPEN" or pr.state == "CLOSED" or pr.state == "MERGED")
        or type(pr.baseRefName) ~= "string"
        or pr.baseRefName == ""
    then
        invalid()
    end
    local state
    if string.lower(head) ~= lower(input.headSha) then
        state = "superseded"
    elseif pr.state ~= "OPEN" then
        state = "closed"
    else
        return nil
    end
    local display = if state == "superseded"
        then {
            tone = "warning",
            title = "A new commit was pushed",
            detail = shortSha(input.headSha) .. " → " .. shortSha(head),
        }
        else {
            tone = "warning",
            title = "PR #" .. input.prNumber .. " was " .. (if pr.state == "MERGED" then "merged" else "closed"),
            detail = "",
        }
    return {
        state = state,
        repository = name,
        prNumber = input.prNumber,
        headSha = lower(input.headSha),
        currentHeadSha = string.lower(head),
        pullRequestState = pr.state,
        display = display,
    }
end

-- A required check: its name and, when bound, the application that must report it.
local function requirement(context: any, app: any): (string, number?)
    if type(context) ~= "string" or context == "" or utf8.len(context) == nil or utf8.len(context) > 256 then
        invalid()
    end
    if app == nil or app == -1 then
        return context, nil
    end
    if type(app) ~= "number" or app <= 0 or app ~= math.floor(app) then
        invalid()
    end
    return context, app
end

local function key(name: string, app: number?): string
    return name .. "\0" .. (if app then string.format("%d", app) else "")
end

-- Adds a requirement to a set keyed by name and application.
local function add(set: any, name: string, app: number?)
    set[key(name, app)] = { name = name, app = app }
end

local function count(set: any): number
    local total = 0
    for _ in set do
        total += 1
    end
    return total
end

local function signature(set: any): string
    local keys = {}
    for entry in set do
        table.insert(keys, entry)
    end
    table.sort(keys)
    return table.concat(keys, "\1")
end

local function classic(pr: any): any
    local required = {}
    local rule = pr.baseRef and pr.baseRef.branchProtectionRule
    if rule == nil then
        return required
    end
    if type(rule.requiresStatusChecks) ~= "boolean" then
        invalid()
    end
    if not rule.requiresStatusChecks then
        return required
    end
    if type(rule.requiredStatusChecks) ~= "table" then
        invalid()
    end
    for _, check in rule.requiredStatusChecks do
        add(required, requirement(check.context, check.app and check.app.databaseId))
        if count(required) > MAX_REQUIRED then
            unavailable("This PR exceeds the supported limit of required checks.")
        end
    end
    return required
end

-- Required status checks from the branch's active rulesets.
local function rules(name: string, branch: string, instance: string?): any
    local required = {}
    for page = 1, MAX_RULE_PAGES do
        local path = "/repos/" .. name .. "/rules/branches/" .. github.segment(branch)
            .. "?per_page=100&page=" .. page
        local list, failure = github.get(path, instance)
        if type(list) ~= "table" then
            cairn.fail(failure or "Invalid GitHub PR checks response.")
        end
        if #list > 100 then
            invalid()
        end
        for _, rule in list do
            if rule.type == "workflows" then
                unavailable("Required workflow rules are not supported by this wait condition.")
            end
            if rule.type == "required_status_checks" then
                local checks = rule.parameters and rule.parameters.required_status_checks
                if type(checks) ~= "table" then
                    invalid()
                end
                for _, check in checks do
                    add(required, requirement(check.context, check.integration_id))
                    if count(required) > MAX_REQUIRED then
                        unavailable("This PR exceeds the supported limit of required checks.")
                    end
                end
            end
        end
        if #list < 100 then
            return required
        end
    end
    unavailable("This PR exceeds the supported limit of branch rules.")
    return required
end

local BUCKETS = {
    SUCCESS = "success",
    NEUTRAL = "success",
    SKIPPED = "success",
    FAILURE = "failure",
    ERROR = "failure",
    TIMED_OUT = "failure",
    STARTUP_FAILURE = "failure",
    ACTION_REQUIRED = "failure",
    STALE = "failure",
    CANCELLED = "cancelled",
    EXPECTED = "pending",
    PENDING = "pending",
}
local RUNNING = { QUEUED = true, IN_PROGRESS = true, WAITING = true, PENDING = true, REQUESTED = true }
local PRIORITY = { failure = 0, cancelled = 1, pending = 2, success = 3 }

local function evaluate(name: string, input: any, required: any, checks: { any }): any?
    local selected = {}
    local present = {}
    for _, check in checks do
        if type(check.isRequired) ~= "boolean" then
            invalid()
        end
        if check.isRequired then
            local isRun = check.__typename == "CheckRun"
            if not isRun and check.__typename ~= "StatusContext" then
                invalid()
            end
            local checkName, app
            if isRun then
                checkName, app = requirement(check.name, check.checkSuite and check.checkSuite.app and check.checkSuite.app.databaseId)
            else
                checkName, app = requirement(check.context, nil)
                local bindings = {}
                for _, entry in required do
                    if entry.name == checkName then
                        table.insert(bindings, entry)
                    end
                end
                if #bindings == 1 then
                    app = bindings[1].app
                elseif #bindings > 1 then
                    unavailable("A required legacy status has ambiguous application bindings. Inspect the PR checks directly.")
                end
            end
            if isRun and lower(check.checkSuite and check.checkSuite.commit and check.checkSuite.commit.oid) ~= lower(input.headSha) then
                invalid()
            end
            add(present, checkName, app)
            local state
            if isRun then
                if check.status == "COMPLETED" then
                    state = check.conclusion
                elseif RUNNING[check.status] then
                    state = "PENDING"
                else
                    invalid()
                end
            else
                state = check.state
            end
            local bucket = BUCKETS[state]
            if not bucket then
                invalid()
            end
            table.insert(selected, {
                name = checkName,
                type = if isRun then "check_run" else "status_context",
                state = bucket,
            })
        end
    end
    local missing = {}
    for _, entry in required do
        local found = false
        for _, reported in present do
            if reported.name == entry.name and (entry.app == nil or reported.app == entry.app) then
                found = true
                break
            end
        end
        if not found then
            table.insert(missing, entry.name)
        end
    end
    table.sort(missing)
    local failure, cancelled, pending = false, false, #missing
    for _, check in selected do
        if check.state == "failure" then
            failure = true
        elseif check.state == "cancelled" then
            cancelled = true
        elseif check.state == "pending" then
            pending += 1
        end
    end
    if not failure and not cancelled and pending > 0 then
        return nil
    end
    local conclusion = if failure then "failure"
        elseif cancelled then "cancelled"
        elseif #selected == 0 and count(required) == 0 then "no_required_checks"
        else "success"
    table.sort(selected, function(a, b)
        if PRIORITY[a.state] ~= PRIORITY[b.state] then
            return PRIORITY[a.state] < PRIORITY[b.state]
        end
        return a.name < b.name
    end)
    local total, missingCount = #selected, #missing
    local shown = {}
    for index, check in selected do
        if index > 100 then
            break
        end
        table.insert(shown, check)
    end
    local shownMissing = {}
    for index, check in missing do
        if index > 100 - #shown then
            break
        end
        table.insert(shownMissing, check)
    end
    local function named(state: string): string
        local names = {}
        for _, check in selected do
            if check.state == state then
                table.insert(names, check.name)
            end
        end
        if #names > 3 then
            return table.concat({ names[1], names[2], names[3] }, ", ") .. " +" .. (#names - 3)
        end
        return table.concat(names, ", ")
    end
    local requiredCount = total + missingCount
    local display
    if conclusion == "success" then
        display = {
            tone = "ok",
            title = "Required checks passed",
            detail = if requiredCount > 0 then requiredCount .. (if requiredCount == 1 then " check" else " checks") else "",
        }
    elseif conclusion == "failure" then
        display = { tone = "error", title = "Required checks failed", detail = named("failure") }
    elseif conclusion == "cancelled" then
        display = { tone = "warning", title = "Required checks were cancelled", detail = named("cancelled") }
    else
        display = { tone = "warning", title = "This PR has no required checks", detail = "" }
    end
    return {
        state = "completed",
        repository = name,
        prNumber = input.prNumber,
        headSha = lower(input.headSha),
        conclusion = conclusion,
        requiredCount = requiredCount,
        pendingCount = pending,
        checks = cairn.array(shown),
        checksTruncated = requiredCount > 100,
        missingCheckCount = missingCount,
        missingChecks = cairn.array(shownMissing),
        display = display,
    }
end

local function prChecks(event: any, instance: string?): any?
    local input = event.input
    local name = repository(event)
    local initial = query(name, input, CHECKS, nil, instance)
    local result = identity(initial, name, input)
    if result then
        return result
    end
    local required = classic(initial)
    local activeRules = rules(name, initial.baseRefName, instance)
    for entryKey, entry in activeRules do
        required[entryKey] = entry
    end
    if count(required) > MAX_REQUIRED then
        unavailable("This PR exceeds the supported limit of required checks.")
    end
    local initialClassic = signature(classic(initial))
    local page = initial
    local checks = {}
    local cursors = {}
    for index = 0, MAX_CHECK_PAGES - 1 do
        result = identity(page, name, input)
        if result then
            return result
        end
        if page.baseRefName ~= initial.baseRefName or signature(classic(page)) ~= initialClassic then
            return nil
        end
        local commit = page.commits and page.commits.nodes and page.commits.nodes[1] and page.commits.nodes[1].commit
        if not commit or lower(commit.oid) ~= lower(input.headSha) then
            invalid()
        end
        local rollup = commit.statusCheckRollup
        if rollup == nil then
            if index > 0 then
                return nil
            end
            break
        end
        local contexts = rollup.contexts
        if type(contexts) ~= "table" or type(contexts.totalCount) ~= "number" then
            invalid()
        end
        if contexts.totalCount > MAX_CHECK_PAGES * 100 then
            unavailable("This PR exceeds the supported limit of 500 checks.")
        end
        local nodes = contexts.nodes
        if type(nodes) ~= "table" or #nodes > 100 then
            invalid()
        end
        for _, node in nodes do
            table.insert(checks, node)
        end
        local info = contexts.pageInfo
        if type(info) ~= "table" or type(info.hasNextPage) ~= "boolean" then
            invalid()
        end
        if not info.hasNextPage then
            if #checks ~= contexts.totalCount then
                return nil
            end
            break
        end
        local cursor = info.endCursor
        if type(cursor) ~= "string" or cursor == "" or #cursor > 1024 or cursors[cursor] then
            invalid()
        end
        cursors[cursor] = true
        if index + 1 == MAX_CHECK_PAGES then
            unavailable("This PR exceeds the supported limit of check pages.")
        end
        page = query(name, input, CHECKS, cursor, instance)
    end
    if signature(rules(name, initial.baseRefName, instance)) ~= signature(activeRules) then
        return nil
    end
    -- Fence pushes/closure/base changes after fetching pages and rechecking rules.
    local current = query(name, input, IDENTITY, nil, instance)
    result = identity(current, name, input)
    if result then
        return result
    end
    if current.baseRefName ~= initial.baseRefName or signature(classic(current)) ~= initialClassic then
        return nil
    end
    return evaluate(name, input, required, checks)
end

local function waiting(event: any): any
    local input = event.input
    if event.condition == "pr_checks" then
        return {
            title = "Waiting for required checks on PR #" .. input.prNumber,
            short = "PR #" .. input.prNumber .. " checks",
            details = cairn.array({ shortSha(input.headSha) }),
        }
    end
    return {
        title = "Waiting for a GitHub workflow run",
        short = "GitHub workflow",
        details = cairn.array({ "Run " .. string.format("%d", input.runId), shortSha(input.headSha) }),
    }
end

return {
    condition = function(event)
        local instance = github.first(event.instances)
        local observe = if event.condition == "pr_checks" then prChecks else workflow
        local ok, result = pcall(observe, event, instance)
        if not ok then
            if getmetatable(result) == Unavailable then
                return { state = "unavailable", message = result.message }
            end
            error(result, 0)
        end
        if result == nil then
            return { state = "waiting", display = waiting(event) }
        end
        return result
    end,

    -- For tests of the Luau port.
    evaluate = evaluate,
    remote = github.remote,
}
