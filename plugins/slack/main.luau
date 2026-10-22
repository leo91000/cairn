-- Slack owns its protocol and UI; Cairn verifies signatures and routed authority.
local socket = require("./socket")
local setup = require("./setup")
local ingress = require("./ingress")
local conversation = require("./conversation")

return {
    socket = socket.handle,
    start = function(event)
        if event.settings.transport ~= "socket" then
            cairn.instance.status("connected", "Signed HTTP endpoint ready")
        end
    end,
    form = setup.form,
    view = setup.view,
    webhook = ingress.webhook,
    message = conversation.message,
    interaction = conversation.interaction,
    reply = conversation.reply,
}
