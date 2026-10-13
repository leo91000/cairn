//! A source-only Luau interpreter. Run in a dedicated process, never in the manager.
use cairn_plugin_api::{API_VERSION, Frame, MAX_FRAME_BYTES, MAX_SOURCE_BYTES};
use mlua::{Lua, LuaSerdeExt, VmState, serde::SerializeOptions};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    io::{BufRead, Read, Write},
    rc::Rc,
    time::{Duration, Instant},
};

/// Friendly wrappers over the single `cairn.call` bridge. Loaded before the sandbox
/// freezes globals, so packages cannot replace them.
const PRELUDE: &str = r#"
local call = cairn.call
local array_mt = cairn.array_metatable
cairn.array_metatable = nil
-- Empty Lua tables encode as JSON objects; mark the ones meant as arrays.
cairn.array = function(items) return setmetatable(items or {}, array_mt) end
cairn.storage = {
    get = function(key) return call("storage.get", { key = key }) end,
    set = function(key, value) call("storage.set", { key = key, value = value }) end,
    delete = function(key) call("storage.delete", { key = key }) end,
    list = function(prefix) return call("storage.list", { prefix = prefix or "" }) end,
}
cairn.http = {
    request = function(request) return call("http.request", request) end,
}
cairn.timers = {
    after = function(seconds, name, payload)
        return call("timers.after", { seconds = seconds, name = name, payload = payload })
    end,
    cancel = function(id) call("timers.cancel", { id = id }) end,
}
cairn.conversations = {
    submit = function(message) return call("conversations.submit", message) end,
    answer = function(answer) return call("conversations.answer", answer) end,
    cancel = function(target) return call("conversations.cancel", target) end,
}
"#;

/// JSON `null` becomes Lua `nil`, so `if value then` behaves as plugin authors expect.
fn options() -> SerializeOptions {
    SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false)
}

/// Why a host call failed, as seen by the script.
pub enum CallError {
    /// The host refused the call; the script receives this short code as an error.
    Denied(String),
    /// The bridge itself broke; the handler cannot continue.
    Broken(String),
}

type Bridge = Rc<RefCell<Box<dyn FnMut(&str, Value) -> Result<Value, CallError>>>>;

/// One loaded package. Globals and upvalues persist between events.
pub struct Runtime {
    lua: Lua,
    handlers: mlua::Table,
    bridge: Bridge,
    deadline: Rc<Cell<Instant>>,
    budget: Duration,
}

impl Runtime {
    pub fn load(source: &str, budget: Duration) -> mlua::Result<Self> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(mlua::Error::runtime("Plugin source is too large."));
        }
        let lua = Lua::new();
        lua.set_memory_limit(16 * 1024 * 1024)?;
        let deadline = Rc::new(Cell::new(Instant::now() + budget));
        let interrupt = deadline.clone();
        lua.set_interrupt(move |_| {
            if Instant::now() > interrupt.get() {
                return Err(mlua::Error::runtime("Plugin deadline exceeded."));
            }
            Ok(VmState::Continue)
        });
        let bridge: Bridge = Rc::new(RefCell::new(Box::new(|_, _| {
            Err(CallError::Denied("unavailable".into()))
        })));
        let api = lua.create_table()?;
        let call = bridge.clone();
        api.set(
            "call",
            lua.create_function(move |lua, (method, args): (String, mlua::Value)| {
                let args = lua.from_value(args)?;
                let result = (call.borrow_mut())(&method, args);
                match result {
                    Ok(value) => lua.to_value_with(&value, options()),
                    Err(CallError::Denied(code) | CallError::Broken(code)) => {
                        Err(mlua::Error::runtime(code))
                    }
                }
            })?,
        )?;
        api.set("array_metatable", lua.array_metatable())?;
        lua.globals().set("cairn", api)?;
        lua.load(PRELUDE).set_name("prelude").exec()?;
        // Diagnostics and environment mutation never become another output channel.
        for name in ["print", "warn", "getfenv", "setfenv"] {
            lua.globals().set(name, mlua::Value::Nil)?;
        }
        lua.sandbox(true)?;
        let handlers: mlua::Table = lua
            .load(source)
            .set_mode(mlua::chunk::ChunkMode::Text)
            .set_name("plugin")
            .eval()?;
        Ok(Self {
            lua,
            handlers,
            bridge,
            deadline,
            budget,
        })
    }

    /// Runs the handler named `event`; packages without one ignore the event.
    pub fn dispatch<F>(&self, event: &str, payload: Value, call: F) -> mlua::Result<Value>
    where
        F: FnMut(&str, Value) -> Result<Value, CallError> + 'static,
    {
        *self.bridge.borrow_mut() = Box::new(call);
        self.deadline.set(Instant::now() + self.budget);
        let handler: Option<mlua::Function> = self.handlers.get(event)?;
        let Some(handler) = handler else {
            return Ok(Value::Null);
        };
        let output = handler.call::<mlua::Value>(self.lua.to_value_with(&payload, options())?)?;
        self.lua.from_value(output)
    }
}

fn read_frame(reader: &mut impl BufRead) -> Result<Option<Frame>, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_FRAME_BYTES as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "Plugin input failed.")?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err("Invalid plugin frame.".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "Invalid plugin frame.".into())
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

fn host_call(method: &str, args: Value) -> Result<Value, CallError> {
    write_frame(
        &mut std::io::stdout().lock(),
        &Frame::Call {
            method: method.to_owned(),
            args,
        },
    )
    .map_err(CallError::Broken)?;
    match read_frame(&mut std::io::stdin().lock()).map_err(CallError::Broken)? {
        Some(Frame::Reply { value }) => Ok(value),
        Some(Frame::Denied { code }) => Err(CallError::Denied(code)),
        _ => Err(CallError::Broken("Plugin host operation failed.".into())),
    }
}

/// Private worker mode of the Cairn binary: load once, then serve events until the
/// host closes stdin. Errors deliberately omit script data.
pub fn worker() -> Result<(), String> {
    let Some(Frame::Load {
        api_version,
        source,
        deadline_ms,
    }) = read_frame(&mut std::io::stdin().lock())?
    else {
        return Err("Missing plugin package.".into());
    };
    if api_version != API_VERSION {
        write_frame(&mut std::io::stdout().lock(), &Frame::Failure)?;
        return Err("Plugin API version is incompatible.".into());
    }
    let budget = Duration::from_millis(deadline_ms.clamp(10, 120_000));
    let Ok(runtime) = Runtime::load(&source, budget) else {
        write_frame(&mut std::io::stdout().lock(), &Frame::Failure)?;
        return Err("Plugin package failed to load.".into());
    };
    write_frame(&mut std::io::stdout().lock(), &Frame::Ready)?;
    loop {
        let frame = read_frame(&mut std::io::stdin().lock())?;
        let Some(Frame::Event { name, payload }) = frame else {
            return Ok(());
        };
        let frame = match runtime.dispatch(&name, payload, host_call) {
            Ok(value) => Frame::Done { value },
            Err(_) => Frame::Failure,
        };
        write_frame(&mut std::io::stdout().lock(), &frame)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn load(source: &str) -> Runtime {
        Runtime::load(source, Duration::from_secs(5)).unwrap_or_else(|_| panic!("load"))
    }

    #[test]
    fn sandbox_exposes_only_host_capabilities_and_cannot_load_native_code() {
        let runtime = load(
            "return { probe = function(input) return {io = io == nil, package = package == nil, execute = os.execute == nil, env = os.getenv == nil, print = print == nil} end }",
        );
        let output = runtime
            .dispatch("probe", Value::Null, |_, _| {
                Err(CallError::Denied("denied".into()))
            })
            .unwrap();
        for field in ["io", "package", "execute", "env", "print"] {
            assert_eq!(output[field], true);
        }
        let runtime =
            load("return { message = function(input) return cairn.storage.get(input.key) end }");
        let output = runtime
            .dispatch("message", json!({"key": "seen"}), |method, args| {
                assert_eq!(method, "storage.get");
                assert_eq!(args["key"], "seen");
                Ok(json!(42))
            })
            .unwrap();
        assert_eq!(output, 42);
        assert_eq!(
            runtime
                .dispatch("unknown", Value::Null, |_, _| unreachable!())
                .unwrap(),
            Value::Null
        );
    }

    #[test]
    fn state_persists_between_events_and_helpers_are_frozen() {
        let runtime = load(
            "local count = 0 return { tick = function() count += 1 return count end, replace = function() cairn.storage = nil end }",
        );
        for expected in 1..=3 {
            let value = runtime
                .dispatch("tick", Value::Null, |_, _| unreachable!())
                .unwrap();
            assert_eq!(value, expected);
        }
        assert!(
            runtime
                .dispatch("replace", Value::Null, |_, _| unreachable!())
                .is_err()
        );
    }

    #[test]
    fn json_null_is_nil() {
        let runtime = load(
            "return { probe = function(input) return { missing = input.state == nil, stored = cairn.storage.get('x') == nil } end }",
        );
        let output = runtime
            .dispatch("probe", json!({ "state": null }), |_, _| Ok(Value::Null))
            .unwrap();
        assert_eq!(output, json!({ "missing": true, "stored": true }));
        let runtime =
            load("return { probe = function() return { empty = cairn.array(), map = {} } end }");
        let output = runtime
            .dispatch("probe", Value::Null, |_, _| unreachable!())
            .unwrap();
        assert_eq!(output, json!({ "empty": [], "map": {} }));
    }

    #[test]
    fn denied_calls_raise_catchable_codes() {
        let runtime = load(
            "return { message = function() local ok, error = pcall(cairn.http.request, {}) return { ok = ok, error = tostring(error) } end }",
        );
        let output = runtime
            .dispatch("message", Value::Null, |_, _| {
                Err(CallError::Denied("forbidden".into()))
            })
            .unwrap();
        assert_eq!(output["ok"], false);
        assert!(output["error"].as_str().unwrap().contains("forbidden"));
    }

    #[test]
    fn allocations_and_loops_are_bounded_per_event() {
        let runtime = load(
            "return { big = function() return string.rep('x', 32 * 1024 * 1024) end, spin = function() while true do end end, ok = function() return true end }",
        );
        assert!(
            runtime
                .dispatch("big", Value::Null, |_, _| unreachable!())
                .is_err()
        );
        let fast = Runtime::load(
            "return { spin = function() while true do end end, ok = function() return true end }",
            Duration::from_millis(50),
        )
        .unwrap_or_else(|_| panic!("load"));
        assert!(
            fast.dispatch("spin", Value::Null, |_, _| unreachable!())
                .is_err()
        );
        // The next event gets a fresh budget.
        assert_eq!(
            fast.dispatch("ok", Value::Null, |_, _| unreachable!())
                .unwrap(),
            true
        );
    }

    #[test]
    fn bytecode_and_non_table_packages_are_rejected() {
        assert!(Runtime::load("\u{1}invalid bytecode", Duration::from_secs(1)).is_err());
        assert!(Runtime::load("return function() end", Duration::from_secs(1)).is_err());
    }
}
