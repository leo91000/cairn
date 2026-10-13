-- Only explicit bot mentions become work. The host independently checks grants.
return function(input)
    local message = input.message
    if message.author.bot or message.webhook_id ~= nil then
        return nil
    end
    local mentioned = false
    for _, mention in message.mentions do
        if mention.id == input.botId then
            mentioned = true
        end
    end
    if not mentioned then
        return nil
    end
    local content = string.gsub(message.content, "<@!?" .. input.botId .. ">", "")
    content = string.match(content, "^%s*(.-)%s*$")
    if content == "" then
        return nil
    end
    return cairn.call("conversations.submit", {text = content})
end
