local function profile()
    local response = cairn.http.request({ method = "GET", url = "https://gmail.googleapis.com/gmail/v1/users/me/profile", auth = true })
    if response.status ~= 200 or not response.json or type(response.json.emailAddress) ~= "string" then
        return nil, "Google could not verify this mailbox. Reconnect and grant Gmail access."
    end
    return response.json
end

return {
    form = function(input)
        -- The setup steps of the manifest guide the Google Cloud app and the sign-in.
        if not input.credential then
            return {
                errors = if input.submit then { credential = "Connect a Google account before saving." } else {},
            }
        end
        local value, problem = profile()
        if not value then return { errors = { credential = problem } } end
        local email = value.emailAddress
        if input.previous and input.previous.email and input.previous.email ~= email then
            return { errors = { credential = "This authorization belongs to a different mailbox. Add a new account instead of replacing the existing one." } }
        end
        return { values = { email = email }, view = { { type = "alert", tone = "success", title = "Connected as " .. email,
            text = "Agents need an explicit grant for this account. Changing Google permissions requires a new sign-in." } } }
    end,
    test = function(input)
        local value, problem = profile()
        if not value then return { ok = false, message = problem } end
        if value.emailAddress ~= input.instance.settings.email then return { ok = false, message = "The saved mailbox identity changed. Reconnect this account." } end
        return { ok = true, message = "Connected as " .. value.emailAddress }
    end,
    view = function(input)
        return { { type = "facts", items = {
            { label = "Mailbox", value = input.instance.settings.email or "Reconnect to verify" },
            { label = "Sending", value = if input.instance.settings.allowSend then "Allowed" else "Disabled" },
            { label = "Permanent deletion", value = if input.instance.settings.allowDelete then "Allowed" else "Disabled" },
        } } }
    end,
}
