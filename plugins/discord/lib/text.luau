-- Text as Discord measures it: mentions removed, lengths in UTF-16 code units.
local LIMIT = 1900

local function text(content: string, botId: string): string
    local value = string.gsub(content or "", "<@!?" .. botId .. ">", "")
    return string.match(value, "^%s*(.-)%s*$") or ""
end

-- Never split UTF-8 bytes; Discord validates lengths in UTF-16 code units.
local function truncate(value: string, limit: number): string
    local output, units = {}, 0
    for _, code in utf8.codes(value) do
        local size = if code > 0xFFFF then 2 else 1
        if units + size > limit then
            break
        end
        table.insert(output, utf8.char(code))
        units += size
    end
    return table.concat(output)
end

-- Discord counts message length in UTF-16 code units.
local function chunks(value: string): { string }
    local parts, current, units, count = {}, {}, 0, 0
    for _, code in utf8.codes(value) do
        count += 1
        if count > 16000 then
            break
        end
        local size = if code > 0xFFFF then 2 else 1
        if units + size > LIMIT then
            table.insert(parts, table.concat(current))
            current, units = {}, 0
        end
        table.insert(current, utf8.char(code))
        units += size
    end
    if #current > 0 then
        table.insert(parts, table.concat(current))
    end
    return parts
end

-- Thread names: the first line of the request, within Discord's 100-character limit.
local function title(value: string): string
    local line = string.match(value, "^[^\n]*") or ""
    if line == "" then
        return "Conversation"
    end
    return truncate(line, 90)
end

return {
    LIMIT = LIMIT,
    withoutMention = text,
    truncate = truncate,
    chunks = chunks,
    title = title,
}
