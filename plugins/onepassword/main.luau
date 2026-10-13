-- Credentials and live agent grants stay in the Rust host.
return function(input)
    return cairn.call("onepassword.read", input)
end
