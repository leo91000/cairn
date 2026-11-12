//! Rule formulas: CEL expressions completed by Cairn functions (ADR-0075, « Formules »).
//!
//! A formula is pure and its cost is bounded before it runs: its size, its number of
//! nodes and the nesting of its macros are limited, it only calls the CEL standard
//! library, the lists extension, string functions and the Cairn functions below, and
//! it only reads the variables of its place in the rule. Evaluation bounds the rest.
use cel::common::ast::{EntryExpr, Expr, IdedExpr, operators};
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

pub(crate) const MAX_SOURCE: usize = 2000;
pub(crate) const MAX_NODES: usize = 300;
/// `items.map(x, x.tags.exists(t, …))` nests two macros; a third is refused.
pub(crate) const MAX_MACRO_DEPTH: usize = 2;
pub(crate) const MAX_PLACEHOLDERS: usize = 20;
pub(crate) const MAX_TEMPLATE: usize = 10_000;

/// The variables readable at one place of a rule.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    pub names: BTreeSet<String>,
    /// Actions that run before this place, readable as `steps.<id>`.
    pub steps: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallStyle {
    /// `truncate(text, 10)`.
    Global,
    /// `text.truncate(10)`, or the global form.
    Either,
}

pub(crate) struct Function {
    pub name: &'static str,
    /// Arguments, the receiver of a member call included.
    pub arity: std::ops::RangeInclusive<usize>,
    pub style: CallStyle,
    pub signature: &'static str,
    pub description: &'static str,
    pub example: &'static str,
}

/// The Cairn function library. Evaluation lives with the rule engine; the signatures
/// here are what the editor offers and what saving checks.
pub(crate) const FUNCTIONS: &[Function] = &[
    Function {
        name: "truncate",
        arity: 2..=2,
        style: CallStyle::Either,
        signature: "truncate(text: string, length: int) -> string",
        description: "Shortens text to at most `length` characters, ending with … when it was cut.",
        example: "truncate(payload.content, 80)",
    },
    Function {
        name: "chunks",
        arity: 2..=2,
        style: CallStyle::Either,
        signature: "chunks(text: string, size: int) -> list<string>",
        description: "Splits text into pieces of at most `size` characters, at line or word boundaries when possible.",
        example: "chunks(steps.reply.text, 2000)",
    },
    Function {
        name: "formatDate",
        arity: 2..=3,
        style: CallStyle::Global,
        signature: "formatDate(time: timestamp | string, format: string, timezone?: string) -> string",
        description: "Formats a date with strftime fields (%Y-%m-%d %H:%M), in UTC unless an IANA time zone is given.",
        example: "formatDate(now, \"%d/%m/%Y %H:%M\", \"Europe/Paris\")",
    },
    Function {
        name: "lower",
        arity: 1..=1,
        style: CallStyle::Either,
        signature: "lower(text: string) -> string",
        description: "Lowercases text, accents included.",
        example: "lower(payload.content)",
    },
    Function {
        name: "upper",
        arity: 1..=1,
        style: CallStyle::Either,
        signature: "upper(text: string) -> string",
        description: "Uppercases text, accents included.",
        example: "upper(payload.channel.name)",
    },
    Function {
        name: "toJson",
        arity: 1..=1,
        style: CallStyle::Global,
        signature: "toJson(value: dyn) -> string",
        description: "Encodes a value as JSON text.",
        example: "toJson(payload.author)",
    },
    Function {
        name: "fromJson",
        arity: 1..=1,
        style: CallStyle::Global,
        signature: "fromJson(text: string) -> dyn",
        description: "Decodes JSON text into a value.",
        example: "fromJson(steps.fetch.body).items",
    },
];

/// The CEL standard library, the lists extension and the string functions of the
/// CEL strings extension, which Cairn implements with size checks. Namespaced
/// functions (`strings.quote`, `lists.range`) are not offered.
const STANDARD: &[&str] = &[
    "size",
    "contains",
    "startsWith",
    "endsWith",
    "matches",
    "string",
    "int",
    "uint",
    "double",
    "bool",
    "bytes",
    "duration",
    "timestamp",
    "type",
    "dyn",
    "max",
    "min",
    "getFullYear",
    "getMonth",
    "getDate",
    "getDayOfMonth",
    "getDayOfWeek",
    "getDayOfYear",
    "getHours",
    "getMinutes",
    "getSeconds",
    "getMilliseconds",
    "charAt",
    "indexOf",
    "join",
    "lastIndexOf",
    "lowerAscii",
    "upperAscii",
    "replace",
    "split",
    "substring",
    "trim",
    "distinct",
    "flatten",
    "reverse",
    "slice",
    "sort",
];

const OPERATORS: &[&str] = &[
    operators::CONDITIONAL,
    operators::LOGICAL_AND,
    operators::LOGICAL_OR,
    operators::LOGICAL_NOT,
    operators::SUBSTRACT,
    operators::ADD,
    operators::MULTIPLY,
    operators::DIVIDE,
    operators::MODULO,
    operators::EQUALS,
    operators::NOT_EQUALS,
    operators::GREATER_EQUALS,
    operators::LESS_EQUALS,
    operators::GREATER,
    operators::LESS,
    operators::NEGATE,
    operators::INDEX,
    operators::NOT_STRICTLY_FALSE,
    operators::IN,
];

/// Type names a formula may compare with `type(x) == string`.
const TYPES: &[&str] = &[
    "bool",
    "bytes",
    "double",
    "duration",
    "int",
    "list",
    "map",
    "null_type",
    "string",
    "timestamp",
    "type",
    "uint",
];

/// The environment shared by checking and evaluation.
pub(crate) fn environment() -> Arc<cel::Env> {
    static ENVIRONMENT: OnceLock<Arc<cel::Env>> = OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| {
            // The string functions are Cairn's, which check the size of what they build.
            let mut env = cel::Env::stdlib().with_optional_support(false);
            env.add_extension(cel::extensions::lists)
                .expect("lists extension");
            Arc::new(env)
        })
        .clone()
}

pub(crate) fn function(name: &str) -> Option<&'static Function> {
    FUNCTIONS.iter().find(|function| function.name == name)
}

/// Parses a formula without checking what it reads.
pub(crate) fn parse(source: &str) -> Result<IdedExpr, String> {
    if source.trim().is_empty() {
        return Err("Enter a formula.".into());
    }
    if source.chars().count() > MAX_SOURCE {
        return Err(format!("A formula has at most {MAX_SOURCE} characters."));
    }
    environment()
        .parser()
        .parse(source)
        .map_err(|errors| format!("This formula is not valid CEL: {errors}"))
}

/// Parses a formula and checks its cost and everything it calls and reads.
pub(crate) fn check(source: &str, scope: &Scope) -> Result<IdedExpr, String> {
    let expression = parse(source)?;
    let mut walk = Walk {
        scope,
        locals: Vec::new(),
        nodes: 0,
        macros: 0,
    };
    walk.expression(&expression)?;
    Ok(expression)
}

struct Walk<'a> {
    scope: &'a Scope,
    locals: Vec<String>,
    nodes: usize,
    macros: usize,
}

impl Walk<'_> {
    fn expression(&mut self, node: &IdedExpr) -> Result<(), String> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(format!(
                "This formula is too complex: split it into several steps (at most {MAX_NODES} parts)."
            ));
        }
        match &node.expr {
            Expr::Unspecified => Err("This formula is incomplete.".into()),
            Expr::Literal(_) => Ok(()),
            Expr::Ident(name) => self.identifier(name),
            Expr::Select(select) => {
                if let Expr::Ident(root) = &select.operand.expr
                    && root == "steps"
                    && !self.local(root)
                    && !select.test
                    && !self.scope.steps.contains(&select.field)
                {
                    return Err(format!(
                        "No earlier action is named `{}`: steps only holds the results of actions that run before this one.",
                        select.field
                    ));
                }
                self.expression(&select.operand)
            }
            Expr::Call(call) => {
                self.call(&call.func_name, call.target.is_some(), call.args.len())?;
                if let Some(target) = &call.target {
                    self.expression(target)?;
                }
                for argument in &call.args {
                    self.expression(argument)?;
                }
                Ok(())
            }
            Expr::List(list) => {
                for element in &list.elements {
                    self.expression(element)?;
                }
                Ok(())
            }
            Expr::Map(map) => {
                for entry in &map.entries {
                    match &entry.expr {
                        EntryExpr::MapEntry(entry) => {
                            self.expression(&entry.key)?;
                            self.expression(&entry.value)?;
                        }
                        EntryExpr::StructField(_) => {
                            return Err("Messages cannot be built in a formula.".into());
                        }
                    }
                }
                Ok(())
            }
            Expr::Struct(_) => Err("Messages cannot be built in a formula.".into()),
            Expr::Comprehension(comprehension) => {
                self.macros += 1;
                if self.macros > MAX_MACRO_DEPTH {
                    return Err(format!(
                        "A formula nests at most {MAX_MACRO_DEPTH} of all, exists, exists_one, map and filter."
                    ));
                }
                self.expression(&comprehension.iter_range)?;
                self.expression(&comprehension.accu_init)?;
                let before = self.locals.len();
                self.locals.push(comprehension.iter_var.clone());
                if let Some(second) = &comprehension.iter_var2 {
                    self.locals.push(second.clone());
                }
                self.locals.push(comprehension.accu_var.clone());
                self.expression(&comprehension.loop_cond)?;
                self.expression(&comprehension.loop_step)?;
                self.expression(&comprehension.result)?;
                self.locals.truncate(before);
                self.macros -= 1;
                Ok(())
            }
        }
    }

    fn local(&self, name: &str) -> bool {
        self.locals.iter().any(|local| local == name)
    }

    fn identifier(&self, name: &str) -> Result<(), String> {
        if self.local(name) || self.scope.names.contains(name) || TYPES.contains(&name) {
            return Ok(());
        }
        Err(format!("Unknown variable `{name}`."))
    }

    fn call(&self, name: &str, member: bool, arguments: usize) -> Result<(), String> {
        if OPERATORS.contains(&name) {
            return Ok(());
        }
        if let Some(function) = function(name) {
            if member && function.style == CallStyle::Global {
                return Err(format!(
                    "Call {name} as a function: {}.",
                    function.signature
                ));
            }
            let count = arguments + usize::from(member);
            if !function.arity.contains(&count) {
                return Err(format!(
                    "{name} takes different arguments: {}.",
                    function.signature
                ));
            }
            return Ok(());
        }
        if STANDARD.contains(&name) {
            return Ok(());
        }
        Err(format!("Unknown function `{name}`."))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Text(String),
    Expression(String),
}

/// Splits `discord:{{threadId}}` into text and expressions. An expression ends at the
/// first `}}`; write `{{"{{"}}` for literal braces.
pub(crate) fn template(text: &str) -> Result<Vec<Part>, String> {
    if text.chars().count() > MAX_TEMPLATE {
        return Err(format!("A text has at most {MAX_TEMPLATE} characters."));
    }
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        if start > 0 {
            parts.push(Part::Text(rest[..start].to_owned()));
        }
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or("A {{ has no matching }}.")?;
        parts.push(Part::Expression(after[..end].trim().to_owned()));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        parts.push(Part::Text(rest.to_owned()));
    }
    let expressions = parts
        .iter()
        .filter(|part| matches!(part, Part::Expression(_)))
        .count();
    if expressions > MAX_PLACEHOLDERS {
        return Err(format!(
            "A text inserts at most {MAX_PLACEHOLDERS} formulas."
        ));
    }
    Ok(parts)
}

pub(crate) fn check_template(text: &str, scope: &Scope) -> Result<(), String> {
    for part in template(text)? {
        if let Part::Expression(source) = part {
            check(&source, scope).map_err(|error| format!("In {{{{{source}}}}}: {error}"))?;
        }
    }
    Ok(())
}

/// Whether a formula reads a variable or field whose name contains `word`, such as
/// `payload.author.id` or `authorId`. Text inside string literals does not count.
pub(crate) fn reads(source: &str, word: &str) -> bool {
    fn walk(node: &IdedExpr, word: &str) -> bool {
        let named = |name: &str| name.to_lowercase().contains(word);
        match &node.expr {
            Expr::Ident(name) => named(name),
            Expr::Select(select) => named(&select.field) || walk(&select.operand, word),
            Expr::Call(call) => {
                call.target
                    .as_deref()
                    .is_some_and(|target| walk(target, word))
                    || call.args.iter().any(|argument| walk(argument, word))
            }
            Expr::List(list) => list.elements.iter().any(|element| walk(element, word)),
            Expr::Map(map) => map.entries.iter().any(|entry| match &entry.expr {
                EntryExpr::MapEntry(entry) => walk(&entry.key, word) || walk(&entry.value, word),
                EntryExpr::StructField(field) => walk(&field.value, word),
            }),
            Expr::Comprehension(comprehension) => {
                walk(&comprehension.iter_range, word)
                    || walk(&comprehension.loop_step, word)
                    || walk(&comprehension.loop_cond, word)
                    || walk(&comprehension.result, word)
            }
            Expr::Literal(_) | Expr::Struct(_) | Expr::Unspecified => false,
        }
    }
    parse(source).is_ok_and(|expression| walk(&expression, word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            names: ["payload", "steps", "values", "event", "now", "threadId"]
                .into_iter()
                .map(String::from)
                .collect(),
            steps: ["post"].into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn accepts_payload_fields_steps_macros_and_cairn_functions() {
        for source in [
            "payload.author.id == '42'",
            "threadId",
            "steps.post.messageId",
            "payload.tags.exists(t, t == 'urgent')",
            "payload.items.map(i, i.tags.filter(t, t.startsWith('a')))",
            "truncate(payload.content, 80)",
            "payload.content.truncate(80)",
            "chunks(payload.content, 2000).size() > 1",
            "formatDate(now, '%Y', 'Europe/Paris')",
            "'abc'.upperAscii().split('B')",
            "has(payload.thread) ? payload.thread.id : payload.channel.id",
            "type(payload.count) == int",
            "payload.author.id in ['1', '2']",
            "{'text': payload.content, 'n': 1}",
        ] {
            check(source, &scope()).unwrap_or_else(|error| panic!("{source}: {error}"));
        }
    }

    #[test]
    fn refuses_unknown_names_unbounded_nesting_and_late_steps() {
        let cases = [
            ("author == '1'", "Unknown variable `author`"),
            ("steps.later.id", "No earlier action is named `later`"),
            ("system('ls')", "Unknown function `system`"),
            ("lists.range(1000000)", "Unknown function `range`"),
            ("formatDate(now)", "formatDate takes different arguments"),
            ("now.formatDate('%Y')", "Call formatDate as a function"),
            (
                "payload.a.all(x, payload.b.all(y, payload.c.all(z, true)))",
                "nests at most 2",
            ),
            ("payload.", "not valid CEL"),
            ("payload.a.?b", "not valid CEL"),
        ];
        for (source, expected) in cases {
            let error = check(source, &scope()).unwrap_err();
            assert!(error.contains(expected), "{source}: {error}");
        }
        let long = (0..400).map(|_| "1").collect::<Vec<_>>().join("+");
        assert!(check(&long, &scope()).unwrap_err().contains("too complex"));
        assert!(
            check(&"a".repeat(MAX_SOURCE + 1), &scope())
                .unwrap_err()
                .contains("at most")
        );
    }

    #[test]
    fn comprehension_variables_stay_local() {
        assert!(check("payload.a.map(x, x) == [] && x", &scope()).is_err());
    }

    #[test]
    fn templates_split_text_and_formulas() {
        assert_eq!(
            template("discord:{{ threadId }}!").unwrap(),
            vec![
                Part::Text("discord:".into()),
                Part::Expression("threadId".into()),
                Part::Text("!".into()),
            ]
        );
        assert_eq!(
            template("{{\"{{\"}}").unwrap(),
            vec![Part::Expression("\"{{\"".into())]
        );
        assert!(template("{{ open").is_err());
        check_template("Hello {{payload.author.name}}", &scope()).unwrap();
        assert!(
            check_template("Hello {{author}}", &scope())
                .unwrap_err()
                .contains("In {{author}}")
        );
        let many = "{{1}}".repeat(MAX_PLACEHOLDERS + 1);
        assert!(template(&many).is_err());
    }

    #[test]
    fn reading_the_author_ignores_text_that_merely_names_it() {
        assert!(reads("payload.author.id == '1'", "author"));
        assert!(reads("authorId in ['1']", "author"));
        assert!(reads("payload.items.exists(i, i.author == 'x')", "author"));
        assert!(!reads("payload.content.contains('author')", "author"));
        assert!(!reads("payload.channel == 'support'", "author"));
    }
}
