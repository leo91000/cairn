-- Slack's Socket Mode protocol. Credentials are injected by Cairn; temporary
-- connection URLs stay only in this worker and are never written to storage.
local api = require("./lib/api")
local ingress = require("./ingress")
local controls = require("./controls")
local settings = nil
local ready = false
local retry = 1

local function later(seconds)
    ready = false
    cairn.instance.status("connecting", "Waiting to reconnect to Slack")
    cairn.socket.after("connect", seconds * 1000)
end

local function connect()
    -- A response can fail before any socket or timer exists (for example when
    -- Slack closes an HTTP response halfway through). Retain a recovery path.
    local received, response = pcall(api.request, "POST", "apps.connections.open", {}, { secret = "appToken", prefix = "Bearer " })
    if not received then
        later(retry)
        retry = math.min(60, retry * 2)
        return
    end
    if response.status == 429 then
        local seconds = tonumber(response.headers["retry-after"])
        if not seconds or seconds < 0 or seconds >= 86400 then
            cairn.fail("Slack returned an invalid Socket Mode retry delay. Review the app configuration.")
        end
        later(math.max(1, math.ceil(seconds) + 1))
        return
    end
    if response.status ~= 200 or type(response.json) ~= "table" then
        later(retry)
        retry = math.min(60, retry * 2)
        return
    end
    if response.json.ok ~= true then
        local code = response.json.error
        if table.find({ "invalid_auth", "not_authed", "token_revoked", "token_expired", "account_inactive", "not_allowed_token_type", "missing_scope" }, code) then
            cairn.fail("Slack refused the app-level Socket Mode credential. Check the app token and connections:write permission.")
        end
        later(retry)
        retry = math.min(60, retry * 2)
        return
    end
    -- The host enforces WSS and the manifest host list, before connecting; redirects are not followed.
    if type(response.json.url) ~= "string" then
        cairn.fail("Slack did not provide a Socket Mode connection URL.")
    end
    ready = false
    cairn.socket.connect({
        url = response.json.url,
        eventTimeout = 2500,
        -- RFC 6455 health checks continue while an envelope handler awaits HTTP.
        -- Match the Slack SDK's client-Pong and server-Ping timeout defaults.
        heartbeat = { interval = 5000, timeout = 5000, peerTimeout = 30000 },
    })
end

local function message(event)
    if not event.text then
        cairn.fail("Slack sent a Socket Mode event larger than the supported limit.")
    end
    local ok, envelope = pcall(cairn.json.decode, event.text)
    if not ok or type(envelope) ~= "table" then
        return
    end
    if envelope.type == "hello" then
        if type(envelope.connection_info) ~= "table" or envelope.connection_info.app_id ~= settings.appId then
            cairn.fail("The Socket Mode connection belongs to a different Slack app.")
        end
        ready = true
        retry = 1
        cairn.socket.cancel("hello")
        cairn.instance.status("connected", "Slack Socket Mode ready")
        return
    end
    if envelope.type == "disconnect" then
        cairn.socket.close()
        later(1)
        return
    end
    if not ready or type(envelope.envelope_id) ~= "string" or #envelope.envelope_id > 200 or #envelope.envelope_id == 0 then
        return
    end
    local payload = envelope.payload
    if type(payload) ~= "table" then
        return
    end
    local context = { instance = { settings = settings } }
    local handled, result = pcall(function()
        if envelope.type == "events_api" and payload.type == "event_callback" then
            return ingress.accept(context, payload)
        elseif envelope.type == "interactive" and payload.type == "block_actions" then
            return controls.webhook(context, payload)
        end
        -- The generated manifest currently requests mentions and reset controls.
        return { status = 200 }
    end)
    if not handled then
        -- One route/resource can be revoked without invalidating the app's
        -- transport. Refuse only this input. ACK itself still rechecks the root
        -- owner, instance and package, so a global revocation stops the worker.
        local code = tostring(result)
        if string.find(code, "forbidden", 1, true) then
            result = { status = 403 }
        elseif string.find(code, "not_found", 1, true) then
            result = { status = 404 }
        elseif string.find(code, "invalid", 1, true) then
            result = { status = 400 }
        else
            result = { status = 503 }
        end
    end
    -- Busy or uncertain admission is retried by Slack. Never ACK work before its
    -- visibility and native requester/route checks have durably accepted it.
    if result.status < 500 and result.status ~= 409 then
        cairn.socket.send(cairn.json.encode({ envelope_id = envelope.envelope_id }))
    end
end

return { handle = function(event)
    if event.type == "start" then
        settings = event.settings
        if settings.transport == "socket" then
            connect()
        end
        return
    end
    if not settings or settings.transport ~= "socket" then
        return
    end
    if event.type == "open" then
        cairn.socket.after("hello", 10000)
    elseif event.type == "message" then
        message(event)
    elseif event.type == "close" then
        later(retry)
        retry = math.min(60, retry * 2)
    elseif event.type == "tick" and event.name == "connect" then
        connect()
    elseif event.type == "tick" and event.name == "hello" and not ready then
        cairn.socket.close()
        later(retry)
    end
end }
