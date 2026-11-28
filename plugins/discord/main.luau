-- The Discord bot, entirely in Luau. Its Gateway session admits only messages for this
-- bot (mentions, and with Message Content its threads) in the configured channel; Cairn
-- keeps those of allowed people and checks every effect. It also emits what happens in
-- the server as hooks for rules.
local gateway = require("./gateway")
local setup = require("./setup")
local conversation = require("./conversation")
local resources = require("./resources")

return {
    -- Transport worker.
    socket = gateway.socket,
    -- Owner: the instance form, its test and its panel.
    form = setup.form,
    test = setup.test,
    view = setup.view,
    -- The server, channels, threads and people hooks designate.
    resources = resources.resources,
    -- Admitted messages and the agent's work.
    message = conversation.message,
    interaction = conversation.interaction,
    access_requested = conversation.access_requested,
    progress = conversation.progress,
    question = conversation.question,
    reply = conversation.reply,
}
