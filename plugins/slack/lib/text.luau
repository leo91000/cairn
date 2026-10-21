-- Slack recommends short top-level text. Bound each escaped chunk to 3,000
-- UTF-8 bytes: also below its character limit, without splitting code points
-- or the entities Slack requires for literal &, < and >.
return {
    chunks = function(text)
        local chunks = {}
        local parts = {}
        local bytes = 0
        for _, codepoint in utf8.codes(text) do
            local token = if codepoint == 38 then "&amp;"
                elseif codepoint == 60 then "&lt;"
                elseif codepoint == 62 then "&gt;"
                else utf8.char(codepoint)
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
