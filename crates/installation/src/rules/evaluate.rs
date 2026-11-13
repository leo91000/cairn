//! Evaluating rule values and formulas, with a bounded cost.
//!
//! Before a formula runs, Cairn rewrites its tree so that every macro range counts
//! against an iteration budget and every function or operator result is checked against
//! a size limit and counted against a budget of produced bytes. String functions that can
//! grow a value check its size before building it. Together with the checks made when the
//! rule is saved, one evaluation reads at most [`MAX_ITERATIONS`] items, never holds an
//! intermediate value above [`MAX_BYTES`] and produces at most [`MAX_PRODUCED`] bytes.
use super::formula::{self, Part};
use cairn_contracts::RuleValue;
use cel::{
    Context, ExecutionError, FunctionContext, Value,
    common::ast::{CallExpr, EntryExpr, Expr, IdedExpr},
    extractors::This,
};
use serde_json::Map;
use serde_json::Value as Json;
use std::{cell::Cell, collections::BTreeMap, sync::Arc};

/// Items that the macros of one evaluation may iterate, nested ones included.
pub(crate) const MAX_ITERATIONS: usize = 20_000;
/// The largest intermediate or final value, measured as JSON text.
pub(crate) const MAX_BYTES: usize = 256 * 1024;
/// All the values one evaluation produces, added up.
pub(crate) const MAX_PRODUCED: usize = 16 * 1024 * 1024;

const RANGE: &str = "__cairn_range";
const BOUND: &str = "__cairn_bound";

thread_local! {
    static ITERATIONS: Cell<usize> = const { Cell::new(0) };
    static PRODUCED: Cell<usize> = const { Cell::new(0) };
    static DEADLINE: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// The whole pure planning step shares a deadline, including successive formulas.
/// Checking it inside the interpreter and CEL callbacks also retires a blocking job
/// if its async caller has already reached the timeout.
pub(crate) fn with_deadline<T>(deadline: std::time::Instant, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<std::time::Instant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DEADLINE.with(|current| current.set(self.0));
        }
    }
    let _restore = Restore(DEADLINE.with(|current| current.replace(Some(deadline))));
    run()
}

pub(crate) fn check_deadline() -> Outcome<()> {
    let expired = DEADLINE.with(|deadline| {
        deadline
            .get()
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
    });
    if expired {
        Err("Evaluating this step exceeded its time limit.".into())
    } else {
        Ok(())
    }
}

/// The variables of one place in a triggering, as JSON.
#[derive(Clone, Debug, Default)]
pub(crate) struct Variables(pub BTreeMap<String, Json>);

impl Variables {
    pub(crate) fn insert(&mut self, name: &str, value: Json) {
        self.0.insert(name.to_owned(), value);
    }
}

type Outcome<T> = std::result::Result<T, String>;

fn size(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len() + 2,
        Value::Bytes(bytes) => bytes.len() * 4 / 3 + 2,
        Value::List(items) => items.iter().map(size).sum::<usize>() + items.len() + 2,
        Value::Map(map) => {
            map.map
                .iter()
                .map(|(key, value)| key.to_string().len() + 4 + size(value))
                .sum::<usize>()
                + 2
        }
        _ => 8,
    }
}

fn too_large() -> ExecutionError {
    ExecutionError::function_error(
        "limit",
        format!("A formula value grew above {} KiB.", MAX_BYTES / 1024),
    )
}

fn bound(value: Value) -> Result<Value, ExecutionError> {
    check_deadline().map_err(|error| ExecutionError::function_error("limit", error))?;
    let size = size(&value);
    if size > MAX_BYTES {
        return Err(too_large());
    }
    let produced = PRODUCED.with(|produced| {
        let total = produced.get() + size;
        produced.set(total);
        total
    });
    if produced > MAX_PRODUCED {
        return Err(ExecutionError::function_error(
            "limit",
            format!(
                "A formula produced more than {} MiB.",
                MAX_PRODUCED / 1024 / 1024
            ),
        ));
    }
    Ok(value)
}

fn range(value: Value) -> Result<Value, ExecutionError> {
    check_deadline().map_err(|error| ExecutionError::function_error("limit", error))?;
    let count = match &value {
        Value::List(items) => items.len(),
        Value::Map(map) => map.map.len(),
        _ => 0,
    };
    let total = ITERATIONS.with(|used| {
        let total = used.get() + count;
        used.set(total);
        total
    });
    if total > MAX_ITERATIONS {
        return Err(ExecutionError::function_error(
            "limit",
            format!("A formula iterated over more than {MAX_ITERATIONS} items."),
        ));
    }
    Ok(value)
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::List(_) => "a list",
        Value::Map(_) => "a map",
        Value::Int(_) | Value::UInt(_) | Value::Float(_) => "a number",
        Value::String(_) => "text",
        Value::Bytes(_) => "bytes",
        Value::Bool(_) => "a boolean",
        Value::Duration(_) => "a duration",
        Value::Timestamp(_) => "a timestamp",
        Value::Null => "null",
        _ => "another value",
    }
}

/// The arguments after the text a function applies to, called as `f(text, …)` or
/// `text.f(…)`.
fn rest<'a>(ftx: &FunctionContext, arguments: &'a [Value]) -> &'a [Value] {
    if ftx.this.is_some() {
        arguments
    } else {
        &arguments[1.min(arguments.len())..]
    }
}

fn argument<'a>(
    ftx: &FunctionContext,
    arguments: &'a [Value],
    index: usize,
) -> Result<&'a Value, ExecutionError> {
    rest(ftx, arguments)
        .get(index)
        .ok_or_else(|| ftx.error("missing argument"))
}

fn text(value: &Value, ftx: &FunctionContext) -> Result<Arc<String>, ExecutionError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        other => Err(ftx.error(format!("expected text, got {}", kind(other)))),
    }
}

fn int(value: &Value, ftx: &FunctionContext) -> Result<i64, ExecutionError> {
    match value {
        Value::Int(number) => Ok(*number),
        Value::UInt(number) => i64::try_from(*number).map_err(|_| ftx.error("number too large")),
        other => Err(ftx.error(format!("expected a number, got {}", kind(other)))),
    }
}

fn truncate(
    ftx: &FunctionContext,
    This(this): This<Value>,
    cel::extractors::Arguments(arguments): cel::extractors::Arguments,
) -> Result<Value, ExecutionError> {
    let text = text(&this, ftx)?;
    let length = usize::try_from(int(argument(ftx, &arguments, 0)?, ftx)?)
        .map_err(|_| ftx.error("length must not be negative"))?;
    if text.chars().count() <= length {
        return Ok(Value::String(text));
    }
    let kept = text
        .chars()
        .take(length.saturating_sub(1))
        .collect::<String>();
    Ok(Value::String(Arc::new(format!("{}…", kept.trim_end()))))
}

/// Pieces of at most `size` characters, cut at a line break, then a space, when one
/// falls in the second half of the piece.
fn chunks(
    ftx: &FunctionContext,
    This(this): This<Value>,
    cel::extractors::Arguments(arguments): cel::extractors::Arguments,
) -> Result<Value, ExecutionError> {
    let text = text(&this, ftx)?;
    let size = usize::try_from(int(argument(ftx, &arguments, 0)?, ftx)?)
        .ok()
        .filter(|size| *size > 0)
        .ok_or_else(|| ftx.error("size must be positive"))?;
    let characters = text.chars().collect::<Vec<_>>();
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < characters.len() {
        let mut end = (start + size).min(characters.len());
        if end < characters.len() {
            let window = &characters[start..end];
            let cut = window
                .iter()
                .rposition(|c| *c == '\n')
                .or_else(|| window.iter().rposition(|c| c.is_whitespace()))
                .filter(|cut| *cut >= size / 2);
            if let Some(cut) = cut {
                end = start + cut + 1;
            }
        }
        let piece = characters[start..end].iter().collect::<String>();
        pieces.push(Value::String(Arc::new(piece)));
        start = end;
    }
    Ok(Value::List(Arc::new(pieces)))
}

fn lower(ftx: &FunctionContext, This(this): This<Value>) -> Result<Value, ExecutionError> {
    Ok(Value::String(Arc::new(text(&this, ftx)?.to_lowercase())))
}

fn upper(ftx: &FunctionContext, This(this): This<Value>) -> Result<Value, ExecutionError> {
    Ok(Value::String(Arc::new(text(&this, ftx)?.to_uppercase())))
}

fn format_date(
    ftx: &FunctionContext,
    arguments: cel::extractors::Arguments,
) -> Result<Value, ExecutionError> {
    let arguments = arguments.0;
    let time = match arguments.first() {
        Some(Value::Timestamp(time)) => *time,
        Some(Value::String(text)) => chrono::DateTime::parse_from_rfc3339(text)
            .map_err(|_| ftx.error("expected an RFC 3339 date"))?,
        Some(Value::Int(seconds)) => chrono::DateTime::from_timestamp(*seconds, 0)
            .ok_or_else(|| ftx.error("date out of range"))?
            .fixed_offset(),
        _ => return Err(ftx.error("expected a timestamp")),
    };
    let format = match arguments.get(1) {
        Some(Value::String(format)) if format.len() <= 200 => format.clone(),
        _ => return Err(ftx.error("expected a format of at most 200 characters")),
    };
    let items = chrono::format::StrftimeItems::new(&format)
        .parse()
        .map_err(|_| ftx.error("invalid date format"))?;
    let rendered = match arguments.get(2) {
        None => time
            .with_timezone(&chrono::Utc)
            .format_with_items(items.iter())
            .to_string(),
        Some(Value::String(zone)) => {
            let zone = zone
                .parse::<chrono_tz::Tz>()
                .map_err(|_| ftx.error("unknown time zone"))?;
            time.with_timezone(&zone)
                .format_with_items(items.iter())
                .to_string()
        }
        Some(_) => return Err(ftx.error("expected an IANA time zone")),
    };
    Ok(Value::String(Arc::new(rendered)))
}

fn to_json(ftx: &FunctionContext, This(value): This<Value>) -> Result<Value, ExecutionError> {
    let json = value.json().map_err(|error| ftx.error(error.to_string()))?;
    Ok(Value::String(Arc::new(json.to_string())))
}

fn from_json(ftx: &FunctionContext, This(value): This<Value>) -> Result<Value, ExecutionError> {
    let text = text(&value, ftx)?;
    if text.len() > MAX_BYTES {
        return Err(too_large());
    }
    let json: Json = serde_json::from_str(&text).map_err(|_| ftx.error("invalid JSON"))?;
    Ok(to_cel(&json))
}

/// JSON integers become CEL `int`, as literals in formulas are, so that they combine.
fn to_cel(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(value) => Value::Bool(*value),
        Json::Number(number) => match (number.as_i64(), number.as_u64()) {
            (Some(value), _) => Value::Int(value),
            (None, Some(value)) => Value::UInt(value),
            _ => Value::Float(number.as_f64().unwrap_or_default()),
        },
        Json::String(text) => Value::String(Arc::new(text.clone())),
        Json::Array(items) => Value::List(Arc::new(items.iter().map(to_cel).collect())),
        Json::Object(fields) => fields
            .iter()
            .map(|(key, value)| (key.clone(), to_cel(value)))
            .collect::<std::collections::HashMap<_, _>>()
            .into(),
    }
}

/// `replace` checks the size of its result before building it: an empty pattern
/// inserts the replacement at every character.
fn replace(
    ftx: &FunctionContext,
    This(this): This<Value>,
    arguments: cel::extractors::Arguments,
) -> Result<Value, ExecutionError> {
    let text = text(&this, ftx)?;
    let arguments = arguments.0;
    let rest = rest(ftx, &arguments);
    let (Some(pattern), Some(replacement)) = (rest.first(), rest.get(1)) else {
        return Err(ftx.error("expected a pattern and a replacement"));
    };
    let pattern = self::text(pattern, ftx)?;
    let replacement = self::text(replacement, ftx)?;
    let limit = match rest.get(2) {
        Some(limit) => usize::try_from(int(limit, ftx)?).ok(),
        None => None,
    };
    let matches = if pattern.is_empty() {
        text.chars().count() + 1
    } else {
        text.matches(pattern.as_str()).count()
    };
    let matches = limit.map_or(matches, |limit| matches.min(limit));
    let length = text.len() + matches * replacement.len();
    if length > MAX_BYTES {
        return Err(too_large());
    }
    let replaced = match limit {
        Some(limit) => text.replacen(pattern.as_str(), &replacement, limit),
        None => text.replace(pattern.as_str(), &replacement),
    };
    Ok(Value::String(Arc::new(replaced)))
}

fn join(
    ftx: &FunctionContext,
    This(this): This<Value>,
    arguments: cel::extractors::Arguments,
) -> Result<Value, ExecutionError> {
    let Value::List(items) = this else {
        return Err(ftx.error("expected a list of texts"));
    };
    let arguments = arguments.0;
    let rest = rest(ftx, &arguments);
    let separator = match rest.first() {
        Some(separator) => text(separator, ftx)?,
        None => Arc::new(String::new()),
    };
    let mut length = 0;
    let mut parts = Vec::with_capacity(items.len());
    for item in items.iter() {
        let part = text(item, ftx)?;
        length += part.len() + separator.len();
        if length > MAX_BYTES {
            return Err(too_large());
        }
        parts.push(part);
    }
    let joined = parts
        .iter()
        .map(|part| part.as_str())
        .collect::<Vec<_>>()
        .join(&separator);
    Ok(Value::String(Arc::new(joined)))
}

fn split(
    ftx: &FunctionContext,
    This(this): This<Value>,
    arguments: cel::extractors::Arguments,
) -> Result<Value, ExecutionError> {
    let text = text(&this, ftx)?;
    let arguments = arguments.0;
    let rest = rest(ftx, &arguments);
    let separator = self::text(
        rest.first()
            .ok_or_else(|| ftx.error("expected a separator"))?,
        ftx,
    )?;
    let limit = match rest.get(1) {
        Some(limit) => Some(int(limit, ftx)?),
        None => None,
    };
    let pieces: Vec<Value> = match limit {
        Some(0) => Vec::new(),
        Some(limit) if limit > 0 => text
            .splitn(
                usize::try_from(limit).unwrap_or(usize::MAX),
                separator.as_str(),
            )
            .map(|piece| Value::String(Arc::new(piece.to_owned())))
            .collect(),
        _ if separator.is_empty() => text
            .chars()
            .map(|c| Value::String(Arc::new(c.to_string())))
            .collect(),
        _ => text
            .split(separator.as_str())
            .map(|piece| Value::String(Arc::new(piece.to_owned())))
            .collect(),
    };
    Ok(Value::List(Arc::new(pieces)))
}

fn trim(ftx: &FunctionContext, This(this): This<Value>) -> Result<Value, ExecutionError> {
    Ok(Value::String(Arc::new(text(&this, ftx)?.trim().to_owned())))
}

fn lower_ascii(ftx: &FunctionContext, This(this): This<Value>) -> Result<Value, ExecutionError> {
    Ok(Value::String(Arc::new(
        text(&this, ftx)?.to_ascii_lowercase(),
    )))
}

fn upper_ascii(ftx: &FunctionContext, This(this): This<Value>) -> Result<Value, ExecutionError> {
    Ok(Value::String(Arc::new(
        text(&this, ftx)?.to_ascii_uppercase(),
    )))
}

fn char_at(
    ftx: &FunctionContext,
    This(this): This<Value>,
    cel::extractors::Arguments(arguments): cel::extractors::Arguments,
) -> Result<Value, ExecutionError> {
    let text = text(&this, ftx)?;
    let index = usize::try_from(int(argument(ftx, &arguments, 0)?, ftx)?)
        .map_err(|_| ftx.error("index out of range"))?;
    let count = text.chars().count();
    if index > count {
        return Err(ftx.error("index out of range"));
    }
    Ok(Value::String(Arc::new(
        text.chars()
            .nth(index)
            .map(String::from)
            .unwrap_or_default(),
    )))
}

/// Character offsets, as CEL counts them.
fn index_of(
    ftx: &FunctionContext,
    this: &Value,
    arguments: &[Value],
    last: bool,
) -> Result<Value, ExecutionError> {
    let text = text(this, ftx)?;
    let needle = self::text(argument(ftx, arguments, 0)?, ftx)?;
    let characters = text.chars().count();
    // The optional offset counts characters, like the result.
    let offset = match rest(ftx, arguments).get(1) {
        Some(offset) => usize::try_from(int(offset, ftx)?)
            .ok()
            .filter(|offset| *offset <= characters)
            .ok_or_else(|| ftx.error("index out of range"))?,
        None if last => characters,
        None => 0,
    };
    let byte = text
        .char_indices()
        .nth(offset)
        .map_or(text.len(), |(byte, _)| byte);
    let found = if last {
        let mut end = (byte + needle.len()).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].rfind(needle.as_str())
    } else {
        text[byte..].find(needle.as_str()).map(|found| byte + found)
    };
    Ok(Value::Int(
        found.map_or(-1, |byte| text[..byte].chars().count() as i64),
    ))
}

fn substring(
    ftx: &FunctionContext,
    This(this): This<Value>,
    arguments: cel::extractors::Arguments,
) -> Result<Value, ExecutionError> {
    let text = text(&this, ftx)?;
    let arguments = arguments.0;
    let rest = rest(ftx, &arguments);
    let characters = text.chars().collect::<Vec<_>>();
    let position = |value: Option<&Value>, default: usize| -> Result<usize, ExecutionError> {
        match value {
            Some(value) => usize::try_from(int(value, ftx)?)
                .ok()
                .filter(|index| *index <= characters.len())
                .ok_or_else(|| ftx.error("index out of range")),
            None => Ok(default),
        }
    };
    let start = position(rest.first(), 0)?;
    let end = position(rest.get(1), characters.len())?;
    if start > end {
        return Err(ftx.error("start after end"));
    }
    Ok(Value::String(Arc::new(
        characters[start..end].iter().collect(),
    )))
}

/// A context with the functions a formula may call and its variables.
fn context(variables: &Variables) -> Outcome<Context<'static, 'static>> {
    let mut context = Context::with_env(formula::environment());
    let add = |name: &str, result: Result<(), cel::DeclarationError>| {
        result.map_err(|error| format!("{name}: {error}"))
    };
    add(RANGE, context.add_function(RANGE, range))?;
    add(BOUND, context.add_function(BOUND, bound))?;
    add("truncate", context.add_function("truncate", truncate))?;
    add("chunks", context.add_function("chunks", chunks))?;
    add("lower", context.add_function("lower", lower))?;
    add("upper", context.add_function("upper", upper))?;
    add(
        "formatDate",
        context.add_function("formatDate", format_date),
    )?;
    add("toJson", context.add_function("toJson", to_json))?;
    add("fromJson", context.add_function("fromJson", from_json))?;
    add("replace", context.add_function("replace", replace))?;
    add("join", context.add_function("join", join))?;
    add("split", context.add_function("split", split))?;
    add("trim", context.add_function("trim", trim))?;
    add(
        "lowerAscii",
        context.add_function("lowerAscii", lower_ascii),
    )?;
    add(
        "upperAscii",
        context.add_function("upperAscii", upper_ascii),
    )?;
    add("charAt", context.add_function("charAt", char_at))?;
    add(
        "indexOf",
        context.add_function(
            "indexOf",
            |ftx: &FunctionContext,
             This(this): This<Value>,
             cel::extractors::Arguments(arguments): cel::extractors::Arguments| {
                index_of(ftx, &this, &arguments, false)
            },
        ),
    )?;
    add(
        "lastIndexOf",
        context.add_function(
            "lastIndexOf",
            |ftx: &FunctionContext,
             This(this): This<Value>,
             cel::extractors::Arguments(arguments): cel::extractors::Arguments| {
                index_of(ftx, &this, &arguments, true)
            },
        ),
    )?;
    add("substring", context.add_function("substring", substring))?;
    for (name, value) in &variables.0 {
        // `now` is a timestamp, so that date functions apply to it.
        let time = (name == "now")
            .then(|| {
                value
                    .as_str()
                    .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            })
            .flatten();
        let value = match time {
            Some(time) => Value::Timestamp(time),
            None => to_cel(value),
        };
        context.add_variable_from_value(name.clone(), value);
    }
    Ok(context)
}

fn call(id: u64, function: &str, inner: IdedExpr) -> IdedExpr {
    IdedExpr {
        id,
        expr: Expr::Call(CallExpr {
            func_name: function.to_owned(),
            target: None,
            args: vec![inner],
        }),
    }
}

/// Calls whose result can be larger than their arguments. Comparisons, logic, `type`,
/// `size`, indexing and other fixed-size results are left as they are, so that their
/// values (a `type`, for instance) keep their own kind.
const GROWING: &[&str] = &[
    "_+_",
    "string",
    "bytes",
    "replace",
    "join",
    "split",
    "chunks",
    "toJson",
    "fromJson",
    "lower",
    "upper",
    "lowerAscii",
    "upperAscii",
    "substring",
    "trim",
    "truncate",
    "formatDate",
    "flatten",
    "sort",
    "distinct",
    "reverse",
    "slice",
];

/// Rewrites a checked formula so that macro ranges count against the iteration
/// budget and every value that can grow is size-checked and counted.
fn instrument(node: IdedExpr) -> IdedExpr {
    let id = node.id;
    match node.expr {
        Expr::Call(mut call) => {
            call.target = call.target.map(|target| Box::new(instrument(*target)));
            call.args = call.args.into_iter().map(instrument).collect();
            let growing = GROWING.contains(&call.func_name.as_str());
            let node = IdedExpr {
                id,
                expr: Expr::Call(call),
            };
            if growing {
                self::call(id, BOUND, node)
            } else {
                node
            }
        }
        Expr::Comprehension(mut comprehension) => {
            // Only the range and the step hold the formula's own code. The macro's
            // accumulator, condition and result keep their shape, so that map and
            // filter still build their result in place.
            let range = instrument(std::mem::take(&mut comprehension.iter_range));
            comprehension.iter_range = self::call(id, RANGE, range);
            comprehension.loop_step = step(std::mem::take(&mut comprehension.loop_step));
            self::call(
                id,
                BOUND,
                IdedExpr {
                    id,
                    expr: Expr::Comprehension(comprehension),
                },
            )
        }
        Expr::Select(mut select) => {
            select.operand = Box::new(instrument(*select.operand));
            IdedExpr {
                id,
                expr: Expr::Select(select),
            }
        }
        Expr::List(mut list) => {
            list.elements = list.elements.into_iter().map(instrument).collect();
            self::call(
                id,
                BOUND,
                IdedExpr {
                    id,
                    expr: Expr::List(list),
                },
            )
        }
        Expr::Map(mut map) => {
            for entry in &mut map.entries {
                if let EntryExpr::MapEntry(entry) = &mut entry.expr {
                    entry.key = instrument(std::mem::take(&mut entry.key));
                    entry.value = instrument(std::mem::take(&mut entry.value));
                }
            }
            self::call(
                id,
                BOUND,
                IdedExpr {
                    id,
                    expr: Expr::Map(map),
                },
            )
        }
        expr => IdedExpr { id, expr },
    }
}

/// A macro's step: its accumulating operators and appended list stay as they are.
/// Each appended item is copied into the result, so it counts against the budgets.
fn step(node: IdedExpr) -> IdedExpr {
    let id = node.id;
    match node.expr {
        Expr::Call(mut call)
            if call.target.is_none()
                && matches!(call.func_name.as_str(), "_+_" | "_?_:_" | "_&&_" | "_||_") =>
        {
            call.args = call.args.into_iter().map(step).collect();
            IdedExpr {
                id,
                expr: Expr::Call(call),
            }
        }
        Expr::List(mut list) => {
            list.elements = list
                .elements
                .into_iter()
                .map(|element| {
                    let id = element.id;
                    self::call(id, BOUND, instrument(element))
                })
                .collect();
            IdedExpr {
                id,
                expr: Expr::List(list),
            }
        }
        expr => instrument(IdedExpr { id, expr }),
    }
}

/// Evaluates the values of one place of a triggering. Its variables are converted
/// once; loop items join them in a child scope for each evaluation.
pub(crate) struct Evaluator {
    context: Context<'static, 'static>,
    scope: formula::Scope,
}

impl Evaluator {
    pub(crate) fn new(variables: &Variables) -> Outcome<Self> {
        Ok(Self {
            context: context(variables)?,
            scope: formula::Scope {
                names: variables.0.keys().cloned().collect(),
                steps: variables
                    .0
                    .get("steps")
                    .and_then(Json::as_object)
                    .map(|steps| steps.keys().cloned().collect())
                    .unwrap_or_default(),
            },
        })
    }

    /// Evaluates one formula. Checking it again guards against a version saved under
    /// earlier, looser limits.
    pub(crate) fn formula(&self, source: &str, items: &Map<String, Json>) -> Outcome<Json> {
        check_deadline()?;
        let mut scope = self.scope.clone();
        scope.names.extend(items.keys().cloned());
        let program = instrument(formula::check(source, &scope)?);
        let mut context = self.context.new_inner_scope();
        for (name, value) in items {
            context.add_variable_from_value(name.clone(), to_cel(value));
        }
        ITERATIONS.with(|used| used.set(0));
        PRODUCED.with(|produced| produced.set(0));
        let value = Value::resolve(&program, &context).map_err(|error| error.to_string())?;
        check_deadline()?;
        let json = value.json().map_err(|error| error.to_string())?;
        if serde_json::to_vec(&json).map_or(true, |bytes| bytes.len() > MAX_BYTES) {
            return Err(too_large().to_string());
        }
        Ok(json)
    }

    /// The text of a template, each `{{expression}}` replaced by its result.
    pub(crate) fn template(&self, text: &str, items: &Map<String, Json>) -> Outcome<Json> {
        let mut rendered = String::new();
        for part in formula::template(text)? {
            check_deadline()?;
            match part {
                Part::Text(text) => rendered.push_str(&text),
                Part::Expression(source) => match self
                    .formula(&source, items)
                    .map_err(|error| format!("In {{{{{source}}}}}: {error}"))?
                {
                    Json::String(text) => rendered.push_str(&text),
                    Json::Null => {}
                    value => rendered.push_str(&value.to_string()),
                },
            }
            if rendered.len() > MAX_BYTES {
                return Err(too_large().to_string());
            }
        }
        Ok(Json::String(rendered))
    }

    pub(crate) fn value(&self, value: &RuleValue, items: &Map<String, Json>) -> Outcome<Json> {
        check_deadline()?;
        match value {
            RuleValue::Literal { value } => Ok(value.clone()),
            RuleValue::Template { text } => self.template(text, items),
            RuleValue::Formula { expression } => self.formula(expression, items),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn formula(source: &str, variables: &Variables) -> Outcome<Json> {
        Evaluator::new(variables)?.formula(source, &Map::new())
    }

    fn template(text: &str, variables: &Variables) -> Outcome<Json> {
        Evaluator::new(variables)?.template(text, &Map::new())
    }

    fn value(value: &RuleValue, variables: &Variables) -> Outcome<Json> {
        Evaluator::new(variables)?.value(value, &Map::new())
    }

    #[test]
    fn reverse_search_offsets_preserve_unicode_boundaries_and_overlapping_matches() {
        for (source, expected) in [
            ("'aé'.lastIndexOf('a', 1)", 0),
            ("'aé'.lastIndexOf('é', 1)", 1),
            ("'ababa'.lastIndexOf('aba', 3)", 2),
            ("'hello'.lastIndexOf('llo', 3)", 2),
            ("'aé'.lastIndexOf('', 1)", 1),
        ] {
            assert_eq!(
                formula(source, &Variables::default()).unwrap(),
                json!(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn an_expired_step_stops_formulas_and_callbacks_and_releases_its_deadline() {
        let evaluator = Evaluator::new(&Variables::default()).unwrap();
        with_deadline(std::time::Instant::now(), || {
            assert!(
                evaluator
                    .formula("1 + 1", &Map::new())
                    .unwrap_err()
                    .contains("time limit")
            );
            assert!(bound(Value::Int(1)).is_err());
            assert!(range(Value::List(Arc::new(vec![]))).is_err());
        });
        assert_eq!(evaluator.formula("1 + 1", &Map::new()).unwrap(), json!(2));
    }

    fn variables() -> Variables {
        let mut variables = Variables::default();
        variables.insert(
            "payload",
            json!({
                "author": "alice",
                "content": "Hello world, this is a long message.",
                "items": [1, 2, 3],
                "thread": "42",
            }),
        );
        variables.insert("thread", json!("42"));
        variables.insert("steps", json!({ "post": { "messageId": "m1" } }));
        variables.insert("values", json!({}));
        variables.insert("event", json!({ "id": "e1" }));
        variables.insert("now", json!("2026-11-12T10:00:00Z"));
        variables
    }

    #[test]
    fn evaluates_formulas_templates_and_cairn_functions() {
        let v = variables();
        let cases = [
            ("payload.author == 'alice'", json!(true)),
            (
                "type(payload.items) == list && type(1) != string",
                json!(true),
            ),
            ("'abcabc'.indexOf('a', 1)", json!(3)),
            ("'abcabc'.lastIndexOf('a')", json!(3)),
            ("'abcabc'.lastIndexOf('a', 2)", json!(0)),
            ("steps.post.messageId", json!("m1")),
            ("payload.items.map(i, i * 2)", json!([2, 4, 6])),
            ("truncate(payload.content, 9)", json!("Hello wo…")),
            (
                "payload.content.truncate(200)",
                json!("Hello world, this is a long message."),
            ),
            (
                "chunks('aaaa bbbb cccc', 10)",
                json!(["aaaa bbbb ", "cccc"]),
            ),
            ("upper('été')", json!("ÉTÉ")),
            ("lower(payload.author)", json!("alice")),
            (
                "formatDate(timestamp(now), '%d/%m/%Y %H:%M', 'Europe/Paris')",
                json!("12/11/2026 11:00"),
            ),
            ("formatDate(now, '%Y')", json!("2026")),
            ("toJson({'a': 1})", json!("{\"a\":1}")),
            ("fromJson('{\"a\":[1]}').a[0]", json!(1)),
            ("'a-b-c'.replace('-', '+')", json!("a+b+c")),
            ("'a-b-c'.split('-')", json!(["a", "b", "c"])),
            ("['a', 'b'].join(', ')", json!("a, b")),
            ("'  x '.trim()", json!("x")),
            ("'héllo'.indexOf('l')", json!(2)),
            ("'hello'.substring(1, 3)", json!("el")),
            ("'Ab'.lowerAscii() + 'Ab'.upperAscii()", json!("abAB")),
            ("size(thread)", json!(2)),
        ];
        for (source, expected) in cases {
            assert_eq!(formula(source, &v), Ok(expected), "{source}");
        }
        assert_eq!(
            template("discord:{{thread}} by {{payload.author}}", &v),
            Ok(json!("discord:42 by alice"))
        );
        assert_eq!(
            value(&RuleValue::Literal { value: json!([1]) }, &v),
            Ok(json!([1]))
        );
    }

    #[test]
    fn loop_items_join_the_variables_of_one_evaluation() {
        let evaluator = Evaluator::new(&variables()).unwrap();
        let mut items = Map::new();
        items.insert("entry".into(), json!({ "name": "x" }));
        assert_eq!(
            evaluator.formula("entry.name + thread", &items),
            Ok(json!("x42"))
        );
        assert!(
            evaluator
                .formula("entry", &Map::new())
                .unwrap_err()
                .contains("Unknown variable")
        );
    }

    #[test]
    fn growth_and_iterations_stop_at_their_limits() {
        let mut v = variables();
        v.insert("big", json!((0..30_000).collect::<Vec<_>>()));
        let blowup = format!("payload.content{}", ".replace('', 'xxxxxxxx')".repeat(12));
        let error = formula(&blowup, &v).unwrap_err();
        assert!(error.contains("grew above"), "{error}");
        let error = formula("big.map(x, x)", &v).unwrap_err();
        assert!(error.contains("iterated over more than"), "{error}");
        let error = formula("payload.items.map(x, big.exists(y, y == x))", &v).unwrap_err();
        assert!(error.contains("iterated over more than"), "{error}");
        let grown = "payload.content.replace('', 'xxxxxxxx').replace('', 'xxxxxxxx').replace('', 'xxxxxxxx')";
        let joined = format!("[{}].join('')", [grown; 10].join(", "));
        let error = formula(&joined, &v).unwrap_err();
        assert!(error.contains("grew above"), "{error}");
        v.insert("long", json!("y".repeat(200_000)));
        v.insert("mid", json!((0..200).collect::<Vec<_>>()));
        let error = formula("mid.map(x, long + 'z').size()", &v).unwrap_err();
        assert!(error.contains("produced more than"), "{error}");
        let error = formula("mid.map(x, long).size()", &v).unwrap_err();
        assert!(error.contains("produced more than"), "{error}");
        assert_eq!(
            formula("payload.items.filter(x, x > 1)", &v),
            Ok(json!([2, 3]))
        );
        assert!(
            formula("unknown", &v)
                .unwrap_err()
                .contains("Unknown variable")
        );
    }
}
