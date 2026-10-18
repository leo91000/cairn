-- Release discovery belongs to this package. Installation is shared by all accounts
-- and never receives their service-account tokens.
local function resolve(input)
    local architectures = { x86_64 = "amd64", aarch64 = "arm64" }
    local arch = architectures[input.platform.arch]
    if input.platform.os ~= "linux" or not arch then
        cairn.fail("1Password CLI supports Linux x86_64 and aarch64 on this installation.")
    end

    local response = cairn.http.request({
        url = "https://app-updates.agilebits.com/check/1/0/CLI2/en/2.0.0/Y",
    })
    local version = response.json and response.json.version
    local major, minor, patch
    if type(version) == "string" then
        major, minor, patch = string.match(version, "^(%d+)%.(%d+)%.(%d+)$")
    end
    if response.status ~= 200 or major ~= "2" or not minor or tonumber(minor) < 39 or not patch then
        cairn.fail("No compatible stable 1Password CLI release was found (requires >=2.39.0 <3).")
    end

    return {
        version = version,
        url = "https://cache.agilebits.com/dist/1P/op2/pkg/v" .. version
            .. "/op_linux_" .. arch .. "_v" .. version .. ".zip",
        format = "zip",
        executable = "op",
    }
end

local function install(input)
    local release = input.release
    -- The official archive has no published SHA-256 sidecar. Cairn records its
    -- downloaded hash in the lock; provenance here relies on official HTTPS.
    local archive = cairn.download({ url = release.url, sha256 = release.sha256 })
    return cairn.archive.executable({
        archive = archive.id,
        format = release.format,
        path = release.executable,
    })
end

return { resolve = resolve, install = install }
