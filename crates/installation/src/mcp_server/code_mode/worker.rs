//! No manager state is linked into the script: only framed JSON host calls.
mod isolation;

use super::{Frame, MAX_FRAME, MAX_RESULT};
use rquickjs::{Context, Function, Promise, Runtime};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Read, Write},
    time::{Duration, Instant},
};

fn read(reader: &mut impl BufRead) -> Result<Frame, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "Code input interrupted.")?;
    if bytes.len() > MAX_FRAME || bytes.last() != Some(&b'\n') {
        return Err("Invalid code input.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid code frame.".into())
}

fn write(frame: &Frame) -> Result<(), String> {
    let bytes = serde_json::to_vec(frame).map_err(|_| "Invalid code output.")?;
    if bytes.len() > MAX_FRAME {
        return Err("Code output is too large.".into());
    }
    let mut output = std::io::stdout().lock();
    output
        .write_all(&bytes)
        .and_then(|()| output.write_all(b"\n"))
        .and_then(|()| output.flush())
        .map_err(|_| "Code output interrupted.".into())
}

fn host(name: String, args: String) -> String {
    let call = || -> Result<Value, String> {
        let args = serde_json::from_str(&args).map_err(|_| "Invalid operation arguments.")?;
        write(&Frame::Call { name, args })?;
        match read(&mut std::io::stdin().lock())? {
            Frame::Reply { value } => Ok(json!({ "ok": true, "value": value })),
            Frame::Denied { message, status } => {
                Ok(json!({ "ok": false, "message": message, "status": status }))
            }
            _ => Err("Invalid host response.".into()),
        }
    };
    call()
        .unwrap_or_else(|message| json!({ "ok": false, "message": message, "status": 502 }))
        .to_string()
}

fn evaluate(code: &str, input: &Value, names: &[String]) -> Result<Value, String> {
    let runtime = Runtime::new().map_err(|_| "Cannot create code runtime.")?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let deadline = Instant::now() + Duration::from_secs(30);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context = Context::full(&runtime).map_err(|_| "Cannot create code context.")?;

    context.with(|ctx| {
        let bridge = Function::new(ctx.clone(), host)
            .map_err(|_| "Cannot create code bridge.")?;
        ctx.globals()
            .set("__cairnHost", bridge)
            .map_err(|_| "Cannot expose code bridge.")?;
        ctx.globals()
            .set("__cairnInput", input.to_string())
            .map_err(|_| "Cannot expose code input.")?;

        let names = serde_json::to_string(names).map_err(|_| "Invalid operation names.")?;
        let prelude = format!(r#"
            (() => {{
                const host = globalThis.__cairnHost;
                delete globalThis.__cairnHost;
                Object.defineProperty(globalThis, "input", {{value: JSON.parse(globalThis.__cairnInput)}});
                delete globalThis.__cairnInput;
                const call = async (name, args = {{}}) => {{
                    const reply = JSON.parse(host(name, JSON.stringify(args)));
                    if (!reply.ok) {{
                        const error = new Error(reply.message);
                        error.status = reply.status;
                        throw error;
                    }}
                    return reply.value;
                }};
                const api = Object.create(null);
                api.call = call;
                for (const name of {names}) api[name] = (args) => call(name, args);
                Object.defineProperty(globalThis, "cairn", {{value: Object.freeze(api)}});
            }})();
        "#);
        ctx.eval::<(), _>(prelude)
            .map_err(|_| "Cannot initialize code bridge.")?;
        isolation::install()?;

        let source = format!(r#"
            (async () => {{
                try {{
                    const value = await (async () => {{
                        {code}
                    }})();
                    return JSON.stringify({{ok: true, value: value ?? null}});
                }} catch (error) {{
                    return JSON.stringify({{ok: false, message: String(error && error.message || error)}});
                }}
            }})()
        "#);
        let promise: Promise = ctx.eval(source)
            .map_err(|_| "Invalid JavaScript async function body.")?;
        let output: String = promise.finish()
            .map_err(|_| "JavaScript execution failed or exceeded its limits.")?;

        if output.len() > MAX_RESULT + 64 {
            return Err("Code result exceeds 1 MiB. Filter results or read bounded pages.".into());
        }
        let envelope: Value = serde_json::from_str(&output)
            .map_err(|_| "Code must return a JSON value.")?;
        if envelope["ok"] != true {
            let message = envelope["message"].as_str()
                .unwrap_or("JavaScript execution failed.");
            return Err(message.chars().take(4096).collect());
        }

        envelope.get("value").cloned()
            .ok_or_else(|| "Code must return a JSON value.".into())
    })
}

/// Private Cairn subcommand. Source, operation names and JSON replies are the only inputs.
pub fn run() -> Result<(), String> {
    isolation::resources()?;
    let Frame::Start { code, input, names } = read(&mut std::io::stdin().lock())? else {
        return Err("Missing code request.".into());
    };
    let frame = match evaluate(&code, &input, &names) {
        Ok(value) => Frame::Done { value },
        Err(message) => Frame::Failure { message },
    };
    write(&frame)
}
