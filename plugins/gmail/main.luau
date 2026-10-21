local api = require("./lib/api")
local mail = require("./lib/mail")
local setup = require("./setup")

local function invoke(input)
    local request = input.input
    if request.operation == "accounts" then
        local accounts = cairn.array()
        for _, instance in input.instances or {} do
            local permissions = {}
            for key, value in instance.settings do
                if type(value) == "boolean" then permissions[key] = value end
            end
            table.insert(accounts, { id = instance.id, name = instance.name, email = instance.settings.email,
                permissions = permissions })
        end
        return { accounts = accounts }
    end
    if request.operation == "methods" then return api.methods(request.method) end
    local instance = api.account(input, request.accountId)
    if request.operation == "request" then
        return api.request(instance, request.method, request.parameters, request.body, request.options)
    end
    local fileMethods = { fileCreate = "create", fileAppend = "append", fileRead = "read", fileDelete = "delete" }
    if fileMethods[request.operation] then
        local args = table.clone(request.file or {})
        args.instance = instance.id
        return cairn.files[fileMethods[request.operation]](args)
    end
    if request.operation == "attachment" then
        return api.request(instance, "users.messages.attachments.get", { messageId = request.messageId, id = request.attachmentId }, nil, {
            responseFile = { jsonPointer = "/data", encoding = "base64url", name = request.name or "attachment", mediaType = request.mediaType or "application/octet-stream" },
        })
    end
    return mail.run(instance, request)
end

local function decode(value, fallback)
    if not value then return fallback end
    local ok, result = pcall(cairn.json.decode, value)
    if not ok or type(result) ~= "table" then cairn.fail("Arguments must contain valid JSON objects.") end
    return result
end

return {
    tool = invoke,
    command = function(input)
        local args = input.arguments
        local request
        if input.command == "call" then request = decode(args.input, {})
        else request = { operation = input.command, accountId = args.account, method = args.method,
            parameters = decode(args.parameters, {}), body = decode(args.body, nil), options = decode(args.options, {}) }
        end
        return invoke({ input = request, instances = input.instances })
    end,
    form = setup.form,
    test = setup.test,
    view = setup.view,
}
