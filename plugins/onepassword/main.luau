-- 1Password through its CLI. Each instance is a service account: Cairn keeps its token
-- and hands it to `op` only through the environment, for one command at a time.
-- Values from `read` go to the agent's command stdout and are never stored.

local FAILED = "1Password request failed. Check the token, vault permissions, reference, and service account limits."

-- Runs `op` for one service account and returns its stdout.
local function run_op(account, args)
    return cairn.exec.run({
        program = "op",
        instance = account,
        args = args,
        env = {
            OP_SERVICE_ACCOUNT_TOKEN = { secret = "token" },
            -- No desktop session, Connect server or persistent cache.
            OP_CACHE = "false",
        },
    })
end

local function nonnegative_integer(value)
    return type(value) == "number" and value >= 0 and value <= 9007199254740991 and value % 1 == 0
end

-- Only known quota fields may leave the CLI. Never forward its stdout/stderr in errors.
local function quota_usage(account)
    local ran, result = pcall(run_op, account, { "service-account", "ratelimit", "--format=json" })
    if not ran or result.status ~= 0 then
        return nil
    end
    local ok, rows = pcall(cairn.json.decode, result.stdout)
    local limits = cairn.array()
    if not ok or type(rows) ~= "table" then
        return nil
    end

    for _, row in rows do
        if type(row) ~= "table" then
            continue
        end
        local known_type = row.type == "account" or row.type == "token"
        local known_action = row.action == "read" or row.action == "write" or row.action == "read_write"
        if known_type and known_action and nonnegative_integer(row.limit) and nonnegative_integer(row.used)
            and nonnegative_integer(row.remaining) and nonnegative_integer(row.reset) then
            table.insert(limits, {
                type = row.type,
                action = row.action,
                limit = row.limit,
                used = row.used,
                remaining = row.remaining,
                reset = row.reset,
            })
        end
    end
    return if #limits > 0 then limits else nil
end

local function quota_failure(account)
    local exhausted
    for _, limit in quota_usage(account) or {} do
        local applies_to_read = (limit.type == "token" and limit.action == "read")
            or (limit.type == "account" and limit.action == "read_write")
        if applies_to_read and limit.remaining == 0 and (not exhausted or limit.reset > exhausted.reset) then
            exhausted = limit
        end
    end
    if not exhausted then
        cairn.fail("1Password rate limit exceeded. Wait for the quota to reset; use cairn plugin onepassword limits --account ACCOUNT_ID to check usage.")
    end

    local scope = if exhausted.type == "account" then "daily account quota" else "hourly service-account quota"
    local message = `1Password {scope} exhausted ({exhausted.used}/{exhausted.limit} requests). Retry after {exhausted.reset} seconds.`
    if exhausted.type == "account" then
        message ..= " This quota is shared by all service accounts; a new token will not reset it."
    end
    cairn.fail(message)
end

local function op(account, args)
    local result = run_op(account, args)
    if result.status == 0 then
        return result.stdout
    end
    local error = string.lower(result.stderr or "")
    if string.find(error, "rate-limit", 1, true) or string.find(error, "rate limit", 1, true)
        or string.find(error, "too many requests", 1, true) or string.find(error, "(429)", 1, true) then
        quota_failure(account)
    end
    cairn.fail(FAILED)
    return result.stdout
end

local function decode(text)
    local ok, value = pcall(cairn.json.decode, text)
    if not ok then
        cairn.fail("Invalid 1Password response.")
    end
    return value
end

-- The service account named by the agent, among those granting it.
local function account(input)
    local id = input.arguments.account
    for _, instance in input.instances do
        if instance.id == id then
            return id
        end
    end
    cairn.fail("This 1Password account is not authorized.")
    return id
end

local function required(value, message)
    if value == nil or value == "" then
        cairn.fail(message)
    end
    return value
end

-- Field metadata of an item, never its values.
local function references(item)
    local fields = cairn.array()
    for _, field in item.fields or {} do
        table.insert(fields, {
            id = field.id,
            label = field.label,
            type = field.type,
            reference = field.reference,
            section = field.section and field.section.label,
        })
    end
    return { fields = fields }
end

local operations = {}

function operations.accounts(input)
    local accounts = cairn.array()
    for _, instance in input.instances do
        table.insert(accounts, { id = instance.id, name = instance.name })
    end
    return { accounts = accounts }
end

function operations.limits(input)
    local limits = quota_usage(account(input))
    if not limits then
        cairn.fail(FAILED)
    end
    return { limits = limits }
end

function operations.vaults(input)
    return { items = decode(op(account(input), { "vault", "list", "--format=json" })) }
end

function operations.items(input)
    local vault = required(input.arguments.vault, "A vault name or ID is required.")
    return { items = decode(op(account(input), { "item", "list", "--vault=" .. vault, "--format=json" })) }
end

function operations.fields(input)
    local vault = required(input.arguments.vault, "A vault name or ID is required.")
    local item = required(input.arguments.item, "An item name or ID is required.")
    local output = op(account(input), { "item", "get", "--vault=" .. vault, "--format=json", "--", item })
    return references(decode(output))
end

function operations.read(input)
    return op(account(input), { "read", "--no-newline", input.arguments.reference })
end

-- One item read for related credentials, without a cache or unrequested fields.
operations["read-fields"] = function(input)
    local encoded = input.arguments.fields
    local ok, fields = pcall(cairn.json.decode, encoded)
    if not string.match(encoded, "^%s*%[") or not ok or type(fields) ~= "table" or #fields == 0 or #fields > 32 then
        cairn.fail("fields must be a JSON array of 1 to 32 distinct field IDs or labels.")
    end
    -- JSON null becomes nil in Luau and could otherwise disappear from the array.
    local inside_string, escaped = false, false
    for index = 1, #encoded do
        local character = string.sub(encoded, index, index)
        if escaped then
            escaped = false
        elseif inside_string and character == "\\" then
            escaped = true
        elseif character == '"' then
            inside_string = not inside_string
        elseif not inside_string and not string.match(character, "[%s%[%],]") then
            cairn.fail("fields must be a JSON array of 1 to 32 distinct field IDs or labels.")
        end
    end
    local requested = {}
    for _, name in fields do
        if type(name) ~= "string" or name == "" or #name > 200 or requested[name] then
            cairn.fail("fields must be a JSON array of 1 to 32 distinct field IDs or labels.")
        end
        requested[name] = true
    end

    local item = decode(op(account(input), {
        "item", "get", "--vault=" .. input.arguments.vault, "--format=json", "--", input.arguments.item,
    }))
    local values = {}
    for _, name in fields do
        local selected
        for _, key in { "id", "label" } do
            for _, field in item.fields or {} do
                if field[key] == name then
                    if selected then
                        cairn.fail("A requested field is ambiguous. Use unique field IDs from the fields command.")
                    end
                    selected = field
                end
            end
            if selected then
                break
            end
        end
        if not selected then
            cairn.fail("A requested field does not exist. Use the fields command to find its ID or label.")
        end
        if selected.type == "OTP" then
            cairn.fail("Use the read command for one-time passwords; read-fields does not return TOTP seeds.")
        end
        values[name] = selected.value or ""
    end
    return cairn.json.encode(values)
end

local function vault_list(instance)
    local vaults = decode(op(instance.id, { "vault", "list", "--format=json" }))
    local items = cairn.array()
    for _, vault in vaults do
        table.insert(items, { title = vault.name or vault.id, subtitle = vault.id })
    end
    return items
end

return {
    command = function(input)
        return operations[input.command](input)
    end,

    -- The `onepassword` tool: metadata only. Secret values never enter the conversation.
    tool = function(input)
        local request = input.input
        local operation = operations[request.operation]
        if request.operation == "read" or request.operation == "read-fields" or operation == nil then
            cairn.fail("Read secrets through the Cairn plugin CLI and pipe them directly to the consuming command.")
        end

        if request.operation ~= "accounts" and request.accountId == nil then
            cairn.fail(`accountId is required for the {request.operation} operation. Call accounts to list authorized accounts, then retry with the chosen account's id as accountId.`)
        end

        return operation({
            arguments = { account = request.accountId, vault = request.vault, item = request.item },
            instances = input.instances,
        })
    end,

    test = function(input)
        local vaults = decode(op(input.instance.id, { "vault", "list", "--format=json" }))
        return { ok = true, message = `Connected · {#vaults} vault{if #vaults == 1 then "" else "s"}` }
    end,

    view = function(input)
        return {
            {
                type = "section",
                title = "Vaults",
                description = "What this service account can read. Agents only see the vaults they ask for.",
                children = {
                    { type = "actions", children = {
                        { type = "button", label = "Show vaults", action = "vaults" },
                    } },
                },
            },
        }
    end,

    action = function(input)
        if input.action ~= "vaults" then
            cairn.fail("Unknown action.")
        end
        return {
            view = {
                {
                    type = "section",
                    title = "Vaults",
                    children = {
                        { type = "list", empty = "This service account cannot read any vault.", items = vault_list(input.instance) },
                        { type = "actions", children = {
                            { type = "button", label = "Refresh", action = "vaults" },
                        } },
                    },
                },
            },
        }
    end,
}
