local api = require("./api")
local mail = {}

local function header(message, name)
    for _, entry in message.payload and message.payload.headers or {} do
        if string.lower(entry.name) == string.lower(name) then return entry.value end
    end
    return nil
end

local function references(value)
    local result = cairn.array()
    for item in (value or ""):gmatch("<([^<>]+)>") do table.insert(result, item) end
    return result
end

function mail.run(instance, request)
    local operation = request.operation
    if operation ~= "send" and operation ~= "reply" and operation ~= "forward" and operation ~= "draft" then
        cairn.fail("Unknown Gmail operation. Call methods for REST operations, then use request.")
    end
    local sending = operation ~= "draft"
    if sending and not instance.settings.allowSend then cairn.fail("Sending is disabled for this account.") end
    if not sending and not instance.settings.allowDrafts then cairn.fail("Draft editing is disabled for this account.") end
    if sending and (type(request.requestId) ~= "string" or #request.requestId == 0 or #request.requestId > 200) then
        cairn.fail("Provide a stable requestId for sending. Reuse it only for an identical request.")
    end
    local message = table.clone(request.message or {})
    message.instance = instance.id
    message.messageKey = request.requestId
    if message.from == nil then message.from = instance.settings.email end
    local sender = if type(message.from) == "string" then message.from else message.from.email
    if sender ~= instance.settings.email then
        local aliases = api.request(instance, "users.settings.sendAs.list")
        if not aliases.ok then return aliases end
        local verified = false
        for _, alias in aliases.data.sendAs or {} do
            if alias.sendAsEmail == sender and alias.verificationStatus == "accepted" then verified = true end
        end
        if not verified then cairn.fail("The From address is not an approved alias of this account.") end
    end
    local temporary
    if operation == "reply" then
        local original = api.request(instance, "users.messages.get", { id = request.messageId, format = "metadata", metadataHeaders = { "Subject", "Message-ID", "References" } })
        if not original.ok then return original end
        local subject = header(original.data, "Subject") or ""
        if message.subject and message.subject ~= subject then cairn.fail("A threaded reply must retain the original Subject. Omit subject or use the exact original value.") end
        local parent = references(header(original.data, "Message-ID"))
        if #parent ~= 1 then cairn.fail("The original has no valid Message-ID. Compose a new message instead.") end
        local chain = references(header(original.data, "References"))
        table.insert(chain, parent[1])
        if #chain > 100 then table.remove(chain, 1) end
        message.inReplyTo = parent
        message.references = chain
        message.subject = subject
        message.threadId = original.data.threadId
    elseif operation == "forward" then
        local original = api.request(instance, "users.messages.get", { id = request.messageId, format = "raw" }, nil, {
            responseFile = { jsonPointer = "/raw", encoding = "base64url", name = "forwarded-message.eml", mediaType = "message/rfc822" },
        })
        if not original.ok then return original end
        temporary = original.file
        message.attachments = table.clone(message.attachments or {})
        table.insert(message.attachments, { id = temporary.id, name = "forwarded-message.eml", mediaType = "message/rfc822" })
    end
    local ok, composed = pcall(cairn.files.composeMime, message)
    if temporary then cairn.files.delete({ instance = instance.id, id = temporary.id }) end
    if not ok then cairn.fail(tostring(composed)) end
    local method = if sending then "users.messages.send" else "users.drafts.create"
    local parameters = {}
    local body = if sending then {} else { message = {} }
    if message.threadId then
        if sending then body.threadId = message.threadId else body.message.threadId = message.threadId end
    end
    if not sending and request.options and request.options.draftId then
        method = "users.drafts.update"
        parameters.id = request.options.draftId
    end
    local resultOk, result = pcall(api.request, instance, method, parameters, body, {
        bodyFile = { id = composed.id, format = "base64url", jsonPointer = if sending then "/raw" else "/message/raw" },
        idempotencyKey = request.requestId,
        idempotencyInput = { operation = operation, messageId = request.messageId, contentDigest = composed.contentDigest, draftId = request.options and request.options.draftId },
    })
    cairn.files.delete({ instance = instance.id, id = composed.id })
    if not resultOk then
        return { ok = false, error = "request_not_confirmed", retryable = false, rfcMessageId = composed.messageId,
            message = "The request was refused or its delivery is unconfirmed. This requestId will not send again automatically. Check Sent by rfc822msgid and reconcile the earlier request before choosing a new requestId." }
    end
    result.rfcMessageId = composed.messageId
    return result
end

return mail
