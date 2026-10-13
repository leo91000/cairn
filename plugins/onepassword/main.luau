-- `cairn plugin onepassword …` from a conversation. Credentials and live agent grants
-- stay in the Rust host; the value goes straight to the command's stdout.
return {
    command = function(input)
        return cairn.call("onepassword.read", input)
    end,
}
