-- 1Password through its CLI. Each instance is a service account: Cairn keeps its token
-- and hands it to `op` only through the environment, for one command at a time.
-- Plaintext values from `read` go only to the consuming command's stdout. Writes, when
-- the owner allows them, reach `op` only through stdin and return references, never values.

local FAILED = "1Password request failed. Check the token, vault permissions, reference, and service account limits."

-- Runs `op` for one service account and returns its stdout.
local function run_op(account, args, stdin)
    return cairn.exec.run({
        program = "op",
        instance = account,
        args = args,
        stdin = stdin,
        env = {
            OP_SERVICE_ACCOUNT_TOKEN = { secret = "token" },
            -- Cairn owns a private encrypted in-memory cache daemon per connection.
            OP_CACHE = "true",
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

-- Hourly token limits are separate for reads and writes; the daily account limit is shared.
local function quota_failure(account, action)
    local exhausted
    for _, limit in quota_usage(account) or {} do
        local applies = (limit.type == "token" and limit.action == action)
            or (limit.type == "account" and limit.action == "read_write")
        if applies and limit.remaining == 0 and (not exhausted or limit.reset > exhausted.reset) then
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

local function op(account, args, stdin)
    local result = run_op(account, args, stdin)
    if result.status == 0 then
        return result.stdout
    end
    local error = string.lower(result.stderr or "")
    if string.find(error, "rate-limit", 1, true) or string.find(error, "rate limit", 1, true)
        or string.find(error, "too many requests", 1, true) or string.find(error, "(429)", 1, true) then
        quota_failure(account, if stdin == nil then "read" else "write")
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

local function allows_write(instance)
    return instance.settings ~= nil and instance.settings.allowWrite == true
end

-- The same account, only when its owner allowed agents to write with it.
local function writable_account(input)
    local id = account(input)
    for _, instance in input.instances do
        if instance.id == id and allows_write(instance) then
            return id
        end
    end
    cairn.fail("Writing is disabled for this 1Password account. The owner can allow it in the service account's permissions.")
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

-- Field metadata of a created or edited item, with where it lives.
local function written(item)
    local result = references(item)
    result.id = item.id
    result.title = item.title
    result.vault = item.vault and item.vault.id
    return result
end

local FIELD_TYPES = { STRING = true, CONCEALED = true, EMAIL = true, URL = true, PHONE = true, OTP = true }
local FIELDS_FORMAT = 'fields must be a JSON array of 1 to 32 {"field", "value", "type"?} objects with distinct fields.'

-- Field assignments: values are never echoed in errors.
local function assignments(encoded)
    local ok, entries = pcall(cairn.json.decode, encoded)
    if not string.match(encoded, "^%s*%[") or not ok or type(entries) ~= "table" or #entries == 0 or #entries > 32 then
        cairn.fail(FIELDS_FORMAT)
    end
    local count = 0
    for _ in entries do
        count += 1
    end
    if count ~= #entries then
        cairn.fail(FIELDS_FORMAT)
    end

    local seen = {}
    for _, entry in entries do
        local valid = type(entry) == "table" and type(entry.field) == "string" and entry.field ~= ""
            and #entry.field <= 200 and type(entry.value) == "string" and not seen[entry.field]
            and (entry.type == nil or FIELD_TYPES[entry.type])
        if not valid then
            cairn.fail(FIELDS_FORMAT)
        end
        seen[entry.field] = true
    end
    return entries
end

-- Applies assignments to an item's fields by ID, then label, like read-fields.
-- Unknown fields are added, concealed unless a type is given; existing fields keep theirs,
-- so an edit never reveals a concealed value.
local function assign(item, entries)
    item.fields = item.fields or cairn.array()
    for _, entry in entries do
        local selected
        for _, key in { "id", "label" } do
            for _, field in item.fields do
                if field[key] == entry.field then
                    if selected then
                        cairn.fail("A field to set is ambiguous. Use unique field IDs from the fields command.")
                    end
                    selected = field
                end
            end
            if selected then
                break
            end
        end

        if selected then
            if entry.type and entry.type ~= selected.type then
                cairn.fail("type applies only to new fields. Leave it out to keep the field's type.")
            end
            selected.value = entry.value
        else
            table.insert(item.fields, { label = entry.field, type = entry.type or "CONCEALED", value = entry.value })
        end
    end
end

local operations = {}

function operations.accounts(input)
    local accounts = cairn.array()
    for _, instance in input.instances do
        table.insert(accounts, { id = instance.id, name = instance.name, write = allows_write(instance) })
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

-- One item read for related credentials, without retaining plaintext or unrequested fields.
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

operations["create-item"] = function(input)
    local id = writable_account(input)
    local entries = if input.arguments.fields then assignments(input.arguments.fields) else {}

    -- The category's template carries its built-in fields, such as username and password.
    local item = decode(op(id, { "item", "template", "get", "--", input.arguments.category }))
    item.title = input.arguments.title
    assign(item, entries)
    if input.arguments.url then
        item.urls = { { href = input.arguments.url, primary = true } }
    end

    local created = op(id, {
        "item", "create", "--vault=" .. input.arguments.vault, "--format=json", "-",
    }, cairn.json.encode(item))
    return written(decode(created))
end

-- Edits rewrite the item from its JSON. It carries no passkey, and attachments, documents
-- or SSH keys may not survive the round trip: only these categories are rewritten.
local REWRITABLE = {
    PASSWORD = true, API_CREDENTIAL = true, SERVER = true, DATABASE = true, SECURE_NOTE = true,
    SOFTWARE_LICENSE = true, WIRELESS_ROUTER = true, EMAIL_ACCOUNT = true,
}

operations["set-fields"] = function(input)
    local id = writable_account(input)
    local entries = assignments(input.arguments.fields)

    -- A cached copy could be stale, and the rewrite would undo newer changes.
    local item = decode(op(id, {
        "item", "get", "--cache=false", "--vault=" .. input.arguments.vault, "--format=json", "--", input.arguments.item,
    }))
    if type(item.id) ~= "string" or item.id == "" then
        cairn.fail("Invalid 1Password response.")
    end
    if not REWRITABLE[item.category] or (type(item.files) == "table" and next(item.files) ~= nil) then
        cairn.fail("set-fields cannot edit this item: Login, Document, SSH Key and other categories, or attachments, could be damaged. Edit it in 1Password, or create a new item.")
    end
    assign(item, entries)

    local edited = op(id, {
        "item", "edit", "--vault=" .. input.arguments.vault, "--format=json", "--", item.id,
    }, cairn.json.encode(item))
    return written(decode(edited))
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
        local metadata = request.operation == "accounts" or request.operation == "limits" or request.operation == "vaults"
            or request.operation == "items" or request.operation == "fields"
        if not metadata or operation == nil then
            cairn.fail("This tool only lists metadata. Read secrets and write items through the Cairn plugin CLI.")
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
