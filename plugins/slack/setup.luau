local api = require("./lib/api")
local NOTICE = "Slack messages and Cairn answers imported here are shared with members of this Cairn installation and follow Cairn retention. Slack edits/deletions do not erase past Cairn work."

local function notice()
    return { type = "alert", tone = "warning", title = "Shared Cairn conversations", text = NOTICE }
end

local function rootForm(event)
    local url = event.setup and event.setup.urls.webhooks.events
    local socketMode = event.values.transport == "socket"
    local manifest = {
        display_information = { name = "Cairn" },
        features = { bot_user = { display_name = "Cairn", always_online = false } },
        oauth_config = { scopes = { bot = { "app_mentions:read", "chat:write", "channels:read", "users:read" } } },
        settings = { socket_mode_enabled = socketMode },
    }
    local ready = event.values.appId and table.find(event.secrets or {}, "signingSecret") ~= nil
    if socketMode or (url and ready) then
        manifest.settings.event_subscriptions = { bot_events = { "app_mention" } }
        manifest.settings.interactivity = { is_enabled = true }
        if not socketMode then
            manifest.settings.event_subscriptions.request_url = url
            manifest.settings.interactivity.request_url = url
        end
    end
    local view = {
        notice(),
        { type = "text", text = "Create a private Slack app from this manifest. Copy its app ID and signing secret from Basic Information. Install it, then add a Workspace resource with its bot token and a public channel. Invite the app there. Activate the completed draft, then configure the workspace’s allowed-person route and agent; activation alone admits nobody. Choose signed HTTP or Socket Mode. Socket Mode also needs an app-level token with connections:write; bot tokens remain on the workspace resources. Other accepted Slack features are still being implemented." },
        { type = "text", text = "Changing the app ID requires verifying each workspace again with a bot token for that app. Workspaces saved without an app identity also need verification before receiving or sending messages." },
        { type = "link", label = "Create a Slack app", url = "https://api.slack.com/apps?new_app=1" },
        { type = "code", text = cairn.json.encode(manifest) },
    }
    if url and not socketMode then
        table.insert(view, { type = "code", text = url })
        table.insert(view, { type = "text", text = "Save the app ID and signing secret in this draft before asking Slack to verify the Events URL. URL verification is supported before activation; ordinary events are not." })
    end
    local errors = {}
    if event.submit and socketMode and not table.find(event.secrets or {}, "appToken") then
        errors.appToken = "Add an app-level token with connections:write for Socket Mode."
    end
    if event.submit and event.values.sharingAcknowledged ~= true then
        errors.sharingAcknowledged = "Acknowledge the shared visibility and retention before activation."
    end
    return { view = cairn.array(view), errors = errors }
end

local function workspaceForm(event)
    local values = event.values
    local guidance = { notice() }
    if not table.find(event.resource.secrets, "botToken") then
        return { view = cairn.array(guidance) }
    end
    local response = api.configuration("auth.test", {})
    local failure = api.problem(response)
    if failure then
        return { errors = { botToken = failure }, view = cairn.array(guidance) }
    end
    local identity = response.json
    if type(identity.team_id) ~= "string" or type(identity.user_id) ~= "string" or type(identity.bot_id) ~= "string" then
        return { errors = { botToken = "Use an installed workspace bot token, not a personal or app-level token." } }
    end
    local savedWorkspace = if event.resource.settings then event.resource.settings.externalId else nil
    if (savedWorkspace ~= nil and savedWorkspace ~= identity.team_id)
        or (values.externalId ~= nil and values.externalId ~= identity.team_id) then
        return { errors = { botToken = "This resource belongs to another workspace. Create a new workspace instead of changing its identity." } }
    end
    response = api.configuration("bots.info", { bot = identity.bot_id })
    failure = api.problem(response)
    if failure or type(response.json.bot) ~= "table" or response.json.bot.app_id ~= event.app.settings.appId then
        return { errors = { botToken = failure or "This bot does not belong to the configured Slack app." } }
    end
    local appId = response.json.bot.app_id
    if not values.channelId then
        return { view = cairn.array(guidance) }
    end
    response = api.configuration("conversations.info", { channel = values.channelId })
    failure = api.problem(response)
    if failure then
        return { errors = { channelId = failure } }
    end
    local channel = response.json.channel
    if type(channel) ~= "table" or channel.id ~= values.channelId or channel.is_private ~= false
        or channel.is_archived ~= false or channel.is_member ~= true or channel.is_shared ~= false
        or channel.is_ext_shared == true or channel.is_org_shared == true or channel.is_pending_ext_shared == true then
        return { errors = { channelId = "For this first delivery, choose an active public, unshared channel that the bot has joined." } }
    end
    table.insert(guidance, { type = "alert", tone = "success", text = "Bot and app verified for " .. identity.team_id .. ". Mentions in the selected channel use only its owner-approved route." })
    return {
        values = { externalId = identity.team_id, appId = appId, botId = identity.user_id, channelName = channel.name or values.channelId },
        view = cairn.array(guidance),
    }
end

return {
    form = function(event)
        return if event.resource then workspaceForm(event) else rootForm(event)
    end,
    view = function()
        return { notice(), { type = "text", text = "The current Slack slice accepts authorized mentions in each configured public channel over signed HTTP or Socket Mode, and returns the answer in its Slack thread. No agent or person is granted access by default. Manage the workspace's Routes to select the agent, project and people." } }
    end,
}
