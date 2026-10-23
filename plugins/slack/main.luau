-- Slack owns its protocol and UI; Cairn verifies signatures and routed authority.
local socket = require("./socket")
local setup = require("./setup")
local ingress = require("./ingress")
local conversation = require("./conversation")
local workControls = require("./work-controls")

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
    question = workControls.question,
    reply = conversation.reply,
}
