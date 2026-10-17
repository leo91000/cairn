local function invoke(event)
    local input = event.input or event.arguments
    local resource = cairn.resources.get({collection = 'workspace', resource = input.resource})
    local team = input.teamOverride or resource.settings.externalId
    local method = input.method or ((input.form or event.operation == 'write') and 'POST' or 'GET')
    local response = cairn.http.request({method = method,
        url = 'https://api.example.test' .. (input.form and '/read-messages' or '/messages?channel=' .. (input.override or input.channel) .. '&team_id=' .. team),
        body = input.raw,
        form = input.form and {channel = input.override or input.channel, team_id = team} or nil,
        headers = {['content-type'] = 'application/x-www-form-urlencoded'},
        auth = {resource = input.resource, secret = 'token', prefix = 'Bearer '}})
    return response.json
end
return {tool = invoke, command = invoke}
