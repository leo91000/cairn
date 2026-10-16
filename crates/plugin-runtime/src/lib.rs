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
    check = function(target) return call("conversations.check", target) end,
    answer = function(answer) return call("conversations.answer", answer) end,
    cancel = function(target) return call("conversations.cancel", target) end,
}
cairn.exec = {
    run = function(request) return call("exec.run", request) end,
}
"#;

/// Longest message a package may show through `cairn.fail`.
const MAX_FAILURE: usize = 300;

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
    failure: Rc<RefCell<Option<String>>>,
}

/// Why a handler did not return a value.
pub enum Failed {
    /// The package called `cairn.fail(message)`: a message meant for the person.
    Rejected(String),
    /// Any other error. Details stay in the worker.
    Error(mlua::Error),
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
        let json = lua.create_table()?;
        json.set(
            "decode",
            lua.create_function(|lua, text: mlua::LuaString| {
                if text.as_bytes().len() > MAX_FRAME_BYTES {
                    return Err(mlua::Error::runtime("invalid"));
                }
                let value: Value = serde_json::from_slice(&text.as_bytes())
                    .map_err(|_| mlua::Error::runtime("invalid"))?;
                lua.to_value_with(&value, options())
            })?,
        )?;
        json.set(
            "encode",
            lua.create_function(|lua, value: mlua::Value| {
                let value: Value = lua.from_value(value)?;
                serde_json::to_string(&value).map_err(|_| mlua::Error::runtime("invalid"))
            })?,
        )?;
        api.set("json", json)?;
        let failure = Rc::new(RefCell::new(None::<String>));
        let failed = failure.clone();
        api.set(
            "fail",
            lua.create_function(move |_, message: String| -> mlua::Result<()> {
                let message = message
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_FAILURE)
                    .collect::<String>();
                *failed.borrow_mut() = Some(message);
                Err(mlua::Error::runtime("failed"))
            })?,
        )?;
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
            failure,
        })
    }

    /// Runs the handler named `event`; packages without one ignore the event.
    pub fn dispatch<F>(&self, event: &str, payload: Value, call: F) -> mlua::Result<Value>
    where
        F: FnMut(&str, Value) -> Result<Value, CallError> + 'static,
    {
        self.run(event, payload, call)
            .map_err(|failed| match failed {
                Failed::Rejected(message) => mlua::Error::runtime(message),
                Failed::Error(error) => error,
            })
    }

    /// Like [`Runtime::dispatch`], telling a package's `cairn.fail` apart from errors.
    pub fn run<F>(&self, event: &str, payload: Value, call: F) -> Result<Value, Failed>
    where
        F: FnMut(&str, Value) -> Result<Value, CallError> + 'static,
    {
        *self.bridge.borrow_mut() = Box::new(call);
        self.deadline.set(Instant::now() + self.budget);
        self.failure.borrow_mut().take();
        let result = (|| {
            let handler: Option<mlua::Function> = self.handlers.get(event)?;
            let Some(handler) = handler else {
                return Ok(Value::Null);
            };
            let output =
                handler.call::<mlua::Value>(self.lua.to_value_with(&payload, options())?)?;
            self.lua.from_value(output)
        })();
        result.map_err(|error| match self.failure.borrow_mut().take() {
            Some(message) => Failed::Rejected(message),
            None => Failed::Error(error),
        })
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
        let frame = match runtime.run(&name, payload, host_call) {
            Ok(value) => Frame::Done { value },
            Err(Failed::Rejected(message)) => Frame::Rejected { message },
            Err(Failed::Error(_)) => Frame::Failure,
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

    #[test]
    fn discord_unicode_titles_and_upload_budgets_are_bounded() {
        let runtime = load(include_str!("../../../plugins/discord/main.luau"));
        let title = format!("{} support", "😀".repeat(60));
        runtime
            .dispatch(
                "message",
                json!({
                    "id": "500", "author": { "id": "300", "name": "Sarah" },
                    "channelId": "200", "botId": "400", "content": title,
                }),
                |method, args| match method {
                    "http.request" if args["url"].as_str().unwrap().ends_with("/threads") => {
                        let title = args["json"]["name"].as_str().unwrap();
                        assert!(title.encode_utf16().count() <= 100);
                        assert_eq!(title.chars().count(), 45);
                        Ok(json!({ "status": 200, "json": { "id": "500" } }))
                    }
                    "http.request" => Ok(json!({ "status": 204 })),
                    "storage.get" => Ok(Value::Null),
                    "storage.list" => Ok(json!([])),
                    "conversations.submit" => Ok(json!({ "chatId": "chat" })),
                    _ => unreachable!("{method}"),
                },
            )
            .unwrap();
        let files = (0..6)
            .map(|n| json!({ "id": n.to_string(), "size": 10 * 1024 * 1024 }))
            .collect::<Vec<_>>();
        let deleted = Rc::new(Cell::new(false));
        let cleaned = deleted.clone();
        runtime.dispatch("reply", json!({
            "deliveryId": "12345678-0000-4000-8000-000000000001", "text": "Done", "status": "completed", "files": files,
            "reply": { "channel": "200", "source": "200", "message": "500", "deferred": true },
        }), move |method, args| {
            match method {
                "storage.get" => Ok(if args["key"] == "thread:500" { json!("500") } else { Value::Null }),
                "storage.list" => Ok(json!([])),
                "storage.delete" => {
                    if args["key"] == "thread:500" { cleaned.set(true); }
                    Ok(Value::Null)
                }
                "http.request" => {
                    if let Some(multipart) = args.get("multipart") {
                        assert_eq!(multipart["files"].as_array().unwrap().len(), 5);
                        assert!(multipart["json"]["content"].as_str().unwrap().contains("1 more deliverable"));
                    }
                    Ok(json!({ "status": 200, "json": { "id": "900" } }))
                }
                _ => unreachable!("{method}"),
            }
        }).unwrap();
        assert!(deleted.get(), "completed deferred routing is cleaned up");
    }

    #[test]
    fn concurrent_questions_in_a_channel_keep_independent_answers() {
        let runtime = load(include_str!("../../../plugins/discord/main.luau"));
        let state = Rc::new(RefCell::new(
            std::collections::BTreeMap::<String, Value>::new(),
        ));
        let calls = Rc::new(RefCell::new(Vec::<(String, Value)>::new()));
        let posted = Rc::new(Cell::new(0));
        let bridge = || {
            let (state, calls, posted) = (state.clone(), calls.clone(), posted.clone());
            move |method: &str, args: Value| {
                calls.borrow_mut().push((method.to_owned(), args.clone()));
                match method {
                    "storage.get" => Ok(state
                        .borrow()
                        .get(args["key"].as_str().unwrap())
                        .cloned()
                        .unwrap_or(Value::Null)),
                    "storage.set" => {
                        state.borrow_mut().insert(
                            args["key"].as_str().unwrap().to_owned(),
                            args["value"].clone(),
                        );
                        Ok(Value::Null)
                    }
                    "storage.delete" => {
                        state.borrow_mut().remove(args["key"].as_str().unwrap());
                        Ok(Value::Null)
                    }
                    "storage.list" => Ok(json!(
                        state
                            .borrow()
                            .keys()
                            .filter(|key| key.starts_with(args["prefix"].as_str().unwrap()))
                            .collect::<Vec<_>>()
                    )),
                    "http.request" => {
                        if args["method"] == "POST" {
                            posted.set(posted.get() + 1);
                        }
                        Ok(
                            json!({ "status": 200, "json": { "id": format!("900{}", posted.get()) } }),
                        )
                    }
                    "conversations.check" | "conversations.answer" => {
                        Ok(json!({ "status": "answering" }))
                    }
                    _ => unreachable!("{method}"),
                }
            }
        };
        let ids = ["a".repeat(64), "b".repeat(64)];
        for (index, id) in ids.iter().enumerate() {
            runtime.dispatch("question", json!({
                "question": { "id": id, "fields": [{ "id": "0", "title": "Choose?", "options": [{ "label": "Yes" }] }] },
                "conversation": format!("chat{index}"), "userId": "300", "deliveryId": format!("delivery{index}"),
                "reply": { "channel": "200", "message": "500", "threaded": false },
            }), bridge()).unwrap();
        }
        assert_eq!(
            state.borrow().len(),
            2,
            "neither question overwrites the other"
        );
        runtime.dispatch("interaction", json!({
            "channelId": "200", "author": { "id": "300", "name": "Sarah" }, "customId": format!("a:{}:0:0", ids[0]),
        }), bridge()).unwrap();
        runtime.dispatch("message", json!({
            "channelId": "200", "botId": "400", "id": "501", "author": { "id": "300", "name": "Sarah" },
            "content": "No", "replyToId": "9002",
        }), bridge()).unwrap();
        let calls = calls.borrow();
        let answers = calls
            .iter()
            .filter(|(method, _)| method == "conversations.answer")
            .map(|(_, args)| args)
            .collect::<Vec<_>>();
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0]["conversation"], "chat0");
        assert_eq!(answers[1]["conversation"], "chat1");
        assert_eq!(answers[1]["answers"], json!({ "0": "No" }));
        assert!(state.borrow().is_empty());
    }
}
