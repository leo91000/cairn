-- Slack owns its protocol and UI; Cairn verifies signatures and routed authority.
local setup = require("./setup")
local ingress = require("./ingress")
local conversation = require("./conversation")

return {
    start = function()
        cairn.instance.status("connected", "Signed HTTP endpoint ready")
    end,
    form = setup.form,
    view = setup.view,
    webhook = ingress.webhook,
    message = conversation.message,
    interaction = conversation.interaction,
    reply = conversation.reply,
}
