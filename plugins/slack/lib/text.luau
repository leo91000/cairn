-- Slack recommends short top-level text. Bound each escaped chunk to 3,000
-- UTF-8 bytes: also below its character limit, without splitting code points
-- or the entities Slack requires for literal &, < and >.
local function escaped(codepoint)
    return if codepoint == 38 then "&amp;"
        elseif codepoint == 60 then "&lt;"
        elseif codepoint == 62 then "&gt;"
        else utf8.char(codepoint)
end

return {
    literal = function(text, limit)
        local parts, characters = {}, 0
        for _, codepoint in utf8.codes(text) do
            local token = escaped(codepoint)
            local length = utf8.len(token)
            if characters + length > limit then
                return table.concat(parts), false
            end
            table.insert(parts, token)
            characters += length
        end
        return table.concat(parts), true
    end,
    chunks = function(text)
        local chunks = {}
        local parts = {}
        local bytes = 0
        for _, codepoint in utf8.codes(text) do
            local token = escaped(codepoint)
            if bytes + #token > 3000 then
                table.insert(chunks, table.concat(parts))
                parts = {}
                bytes = 0
            end
            table.insert(parts, token)
            bytes += #token
        end
        if bytes > 0 then
            table.insert(chunks, table.concat(parts))
        end
        return chunks
    end,
}
