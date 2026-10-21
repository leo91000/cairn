local catalog = require("../catalog")
local api = {}

function api.encode(value)
    return (tostring(value):gsub("([^%w%-_%.~])", function(c)
        return string.format("%%%02X", string.byte(c))
    end))
end

function api.account(input, id)
    if type(id) ~= "string" or id == "" then
        cairn.fail("accountId is required. Call accounts and select the account's id explicitly.")
    end
    for _, instance in input.instances or {} do
        if instance.id == id then return instance end
    end
    cairn.fail("This Gmail account is not authorized for this agent.")
end

local function permission(operation, method)
    if operation == "users.messages.send" or operation == "users.drafts.send" then return "allowSend" end
    if operation == "users.watch" or operation == "users.stop" then return "allowWatch" end
    if operation:find(".settings.cse.", 1, true) or operation:find(".smimeInfo.", 1, true) then return "allowAdmin" end
    if operation:find(".delegates.", 1, true) or operation:find(".forwardingAddresses.", 1, true)
        or operation == "users.settings.updateAutoForwarding" or operation == "users.settings.sendAs.create"
        or operation == "users.settings.sendAs.delete" or operation == "users.settings.sendAs.verify" then return "allowSharing" end
    if operation:find(".settings.", 1, true) then return "allowSettings" end
    if operation:find(".drafts.", 1, true) then return if method == "GET" then nil else "allowDrafts" end
    if operation == "users.messages.delete" or operation == "users.messages.batchDelete" or operation == "users.threads.delete" then return "allowDelete" end
    return if method == "GET" then nil else "allowModify"
end

local function parameter(value, rule, name)
    local expected = rule.type
    if rule.repeated then
        if type(value) ~= "table" then cairn.fail(`{name} must be an array.`) end
        for _, item in value do
            local scalar = table.clone(rule)
            scalar.repeated = false
            parameter(item, scalar, name)
        end
        return
    end
    if expected == "boolean" and type(value) ~= "boolean" then cairn.fail(`{name} must be boolean.`) end
    if expected == "integer" and (type(value) ~= "number" or value ~= math.floor(value) or math.abs(value) > 9007199254740991) then cairn.fail(`{name} must be a safe integer.`) end
    if expected == "string" and type(value) ~= "string" then cairn.fail(`{name} must be text (including 64-bit history IDs).`) end
    if type(value) == "string" and (#value > 10000 or value:find("%c")) then cairn.fail(`Invalid {name}.`) end
    if rule.enum then
        local found = false
        for _, option in rule.enum do if option == value then found = true end end
        if not found then cairn.fail(`Invalid {name}; call methods for the accepted values.`) end
    end
    if rule.minimum and tonumber(value) and tonumber(value) < tonumber(rule.minimum) then cairn.fail(`{name} is too small.`) end
    if rule.maximum and tonumber(value) and tonumber(value) > tonumber(rule.maximum) then cairn.fail(`{name} is too large.`) end
end

function api.request(instance, operation, parameters, body, options)
    local definition = catalog.methods[operation]
    if not definition then cairn.fail("Unknown Gmail operation. Call methods to discover the catalog.") end
    local required = permission(operation, definition.method)
    if required and instance.settings[required] ~= true then cairn.fail(`This account does not allow {required}. The owner can change its permissions and reconnect.`) end
    parameters = table.clone(parameters or {})
    if parameters.userId and parameters.userId ~= "me" then cairn.fail("userId is always me; select a different accountId instead.") end
    parameters.userId = "me"
    local path = definition.path
    local query = {}
    for name, rule in definition.parameters do
        if rule.required and parameters[name] == nil then cairn.fail(`{name} is required for {operation}.`) end
    end
    for name, value in parameters do
        local rule = definition.parameters[name] or catalog.parameters[name]
        -- Credentials are supplied only by the host, never by a query parameter.
        if not rule or name == "access_token" or name == "oauth_token" or name == "key" or name == "callback" then cairn.fail(`Unsupported Gmail parameter: {name}.`) end
        parameter(value, rule, name)
        if rule.location == "path" then
            if value == "." or value == ".." or value:find("/", 1, true) or value:find("\\", 1, true) then cairn.fail("Invalid Gmail resource ID.") end
            path = path:gsub("{" .. name .. "}", function() return api.encode(value) end)
        elseif rule.repeated then
            for _, item in value do table.insert(query, api.encode(name) .. "=" .. api.encode(item)) end
        else
            table.insert(query, api.encode(name) .. "=" .. api.encode(value))
        end
    end
    if path:find("{", 1, true) then cairn.fail("A Gmail path parameter is missing.") end
    if body ~= nil and not definition.request then cairn.fail("This operation does not accept a body.") end
    if body ~= nil and type(body) ~= "table" then cairn.fail("The request body must be an object.") end
    options = options or {}
    local request = {
        instance = instance.id, auth = true, method = definition.method,
        url = "https://gmail.googleapis.com/" .. path .. (if #query > 0 then "?" .. table.concat(query, "&") else ""),
        json = body, bodyFile = options.bodyFile, responseFile = options.responseFile,
        idempotencyKey = options.idempotencyKey, idempotencyInput = options.idempotencyInput,
    }
    local response = cairn.http.request(request)
    if response.status == 429 or response.status == 503 then
        return { ok = false, retryable = definition.method == "GET", status = response.status, retryAfter = response.headers["retry-after"], error = "Gmail is rate limited or temporarily unavailable. Do not repeat a send without checking its result." }
    end
    if response.status < 200 or response.status >= 300 then
        local reason = response.json and response.json.error and response.json.error.status or "request_failed"
        return { ok = false, status = response.status, error = reason, details = response.json and response.json.error, reconnect = response.status == 401 }
    end
    return { ok = true, status = response.status, data = response.json, file = response.file }
end

function api.methods(operation)
    if operation then
        local method = catalog.methods[operation]
        if not method then cairn.fail("Unknown Gmail method.") end
        return { method = method, requestSchema = method.request and catalog.schemas[method.request], schemas = catalog.schemas }
    end
    local methods = cairn.array()
    for name, method in catalog.methods do
        table.insert(methods, { operation = name, method = method.method, permission = permission(name, method.method), request = method.request })
    end
    table.sort(methods, function(a, b) return a.operation < b.operation end)
    return { revision = catalog.revision, source = catalog.source, methods = methods }
end

return api
