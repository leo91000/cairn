-- Slack owns its protocol and UI; Cairn verifies signatures and routed authority.
local socket = require("./socket")
local setup = require("./setup")
local ingress = require("./ingress")
local conversation = require("./conversation")
local workControls = require("./work-controls")
local api = require("./lib/api")

return {
    socket = function(event)
        if event.type == "start" then
            api.bindApp(event.settings.appId)
        end
        return socket.handle(event)
    end,
    start = function(event)
        api.bindApp(event.settings.appId)
        if event.settings.transport ~= "socket" then
            cairn.instance.status("connected", "Signed HTTP endpoint ready")
        end
    end,
    form = setup.form,
    view = setup.view,
    webhook = function(event)
        api.bindApp(event.instance.settings.appId)
        return ingress.webhook(event)
    end,
    message = conversation.message,
    interaction = conversation.interaction,
    question = workControls.question,
    progress = workControls.progress,
    reply = conversation.reply,
}
