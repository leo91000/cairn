-- 1Password through its CLI. Each instance is a service account: Cairn keeps its token
-- and hands it to `op` only through the environment, for one command at a time.
-- Values from `read` go to the agent's command stdout and are never stored.

local FAILED = "1Password request failed. Check the token, vault permissions, reference, and service account limits."

-- Runs `op` for one service account and returns its stdout.
local function op(account, args)
    local result = cairn.exec.run({
        program = "op",
        instance = account,
        args = args,
        env = {
            OP_SERVICE_ACCOUNT_TOKEN = { secret = "token" },
            -- No desktop session, Connect server or persistent cache.
            OP_CACHE = "false",
        },
    })
    if result.status ~= 0 then
        cairn.fail(FAILED)
    end
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
        if request.operation == "read" or operation == nil then
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
