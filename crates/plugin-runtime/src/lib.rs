//! A source-only Luau interpreter. Run in a dedicated process, never in the manager.
use cairn_plugin_api::{API_VERSION, Frame, MAX_FRAME_BYTES, MAX_SOURCE_BYTES};
use mlua::{Lua, LuaSerdeExt, VmState};
use serde_json::Value;
use std::{
    io::{BufRead, Read, Write},
    time::{Duration, Instant},
};

fn read_frame(reader: &mut impl BufRead) -> Result<Frame, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_FRAME_BYTES as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "Plugin input failed.")?;
    if bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err("Invalid plugin frame.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid plugin frame.".into())
}

fn write_frame(writer: &mut impl Write, frame: &Frame) -> Result<(), String> {
    let bytes = serde_json::to_vec(frame).map_err(|_| "Invalid plugin output.")?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err("Plugin output is too large.".into());
    }
    writer
        .write_all(&bytes)
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
        .map_err(|_| "Plugin output failed.".into())
}

pub fn evaluate<F>(source: &str, input: Value, call: F) -> mlua::Result<Value>
where
    F: Fn(&str, Value) -> Result<Value, String> + 'static,
{
    if source.len() > MAX_SOURCE_BYTES {
        return Err(mlua::Error::runtime("Plugin source is too large."));
    }
    let lua = Lua::new();
    lua.set_memory_limit(16 * 1024 * 1024)?;
    let deadline = Instant::now() + Duration::from_secs(35);
    lua.set_interrupt(move |_| {
        if Instant::now() > deadline {
            return Err(mlua::Error::runtime("Plugin deadline exceeded."));
        }
        Ok(VmState::Continue)
    });
    let api = lua.create_table()?;
    api.set(
        "call",
        lua.create_function(move |lua, (method, args): (String, mlua::Value)| {
            let args = lua.from_value(args)?;
            let value = call(&method, args).map_err(mlua::Error::runtime)?;
            lua.to_value(&value)
        })?,
    )?;
    lua.globals().set("cairn", api)?;
    // Diagnostics and environment mutation never become another output channel.
    for name in ["print", "warn", "getfenv", "setfenv"] {
        lua.globals().set(name, mlua::Value::Nil)?;
    }
    lua.sandbox(true)?;
    let handler: mlua::Function = lua
        .load(source)
        .set_mode(mlua::chunk::ChunkMode::Text)
        .set_name("plugin")
        .eval()?;
    let output = handler.call::<mlua::Value>(lua.to_value(&input)?)?;
    lua.from_value(output)
}

/// Private worker mode of the Cairn binary. Errors deliberately omit script data.
pub fn worker() -> Result<(), String> {
    let frame = read_frame(&mut std::io::stdin().lock())?;
    let Frame::Invoke {
        api_version,
        source,
        input,
    } = frame
    else {
        return Err("Missing plugin invocation.".into());
    };
    if api_version != API_VERSION {
        return Err("Plugin API version is incompatible.".into());
    }
    let result = evaluate(&source, input, |method, args| {
        write_frame(
            &mut std::io::stdout().lock(),
            &Frame::Call {
                method: method.to_owned(),
                args,
            },
        )?;
        match read_frame(&mut std::io::stdin().lock())? {
            Frame::Reply { value } => Ok(value),
            _ => Err("Plugin host operation failed.".into()),
        }
    });
    let frame = match result {
        Ok(value) => Frame::Finished { value },
        Err(_) => Frame::Failure,
    };
    write_frame(&mut std::io::stdout().lock(), &frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sandbox_exposes_only_host_capabilities_and_cannot_load_native_code() {
        let output = evaluate("return function(input) return {io = io == nil, package = package == nil, execute = os.execute == nil, env = os.getenv == nil, print = print == nil} end", Value::Null, |_, _| Err("Denied".into())).unwrap();
        for field in ["io", "package", "execute", "env", "print"] {
            assert_eq!(output[field], true);
        }
        let output = evaluate(
            "return function(input) return cairn.call('test', input) end",
            json!({"value": 42}),
            |method, args| {
                assert_eq!(method, "test");
                Ok(args)
            },
        )
        .unwrap();
        assert_eq!(output["value"], 42);
    }

    #[test]
    fn allocations_are_bounded() {
        let output = evaluate(
            "return function(input) return string.rep('x', 32 * 1024 * 1024) end",
            Value::Null,
            |_, _| unreachable!(),
        );
        assert!(output.is_err());
    }

    #[test]
    fn bytecode_is_never_accepted_as_source() {
        let result = evaluate("\u{1}invalid bytecode", Value::Null, |_, _| unreachable!());
        assert!(result.is_err());
    }
}
