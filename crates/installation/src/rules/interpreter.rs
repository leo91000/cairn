//! The tree interpreter. It walks a pinned rule version between actions, evaluating
//! conditions and loops, and stops at each action so that the engine can persist its
//! position before any effect. [`Cursor`] persists control flow; the engine hydrates
//! prior action results from step records.
use super::evaluate::Evaluator;
use cairn_contracts::{RuleCondition, RuleNode, RuleOperator, RuleValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

/// Blocks and iterations one triggering may run, waits excluded.
pub(crate) const MAX_STEPS: u32 = super::validation::MAX_STEPS;
/// Items one loop may iterate.
pub(crate) const MAX_LOOP_ITEMS: usize = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "branch", content = "position", rename_all = "camelCase")]
pub(crate) enum Branch {
    Then(usize),
    Otherwise(usize),
    Body(usize),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Iteration {
    pub name: String,
    pub items: Vec<Json>,
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Frame {
    /// The block this frame runs, from the root of the steps.
    pub path: Vec<Branch>,
    /// The next node of that block.
    pub next: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration: Option<Iteration>,
}

/// Where a triggering stands, plus a transient cache of prior action results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Cursor {
    pub frames: Vec<Frame>,
    /// Hydrated from the latest completed step records; never duplicated in a checkpoint.
    #[serde(skip)]
    pub steps: Map<String, Json>,
    /// Blocks and iterations run so far.
    pub count: u32,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            frames: vec![Frame {
                path: Vec::new(),
                next: 0,
                iteration: None,
            }],
            steps: Map::new(),
            count: 0,
        }
    }
}

fn block<'a>(steps: &'a [RuleNode], path: &[Branch]) -> Option<&'a [RuleNode]> {
    let mut block = steps;
    for branch in path {
        block = match (branch, block) {
            (Branch::Then(position), nodes) => match nodes.get(*position)? {
                RuleNode::If { then, .. } => then,
                _ => return None,
            },
            (Branch::Otherwise(position), nodes) => match nodes.get(*position)? {
                RuleNode::If { otherwise, .. } => otherwise,
                _ => return None,
            },
            (Branch::Body(position), nodes) => match nodes.get(*position)? {
                RuleNode::ForEach { body, .. } => body,
                _ => return None,
            },
        };
    }
    Some(block)
}

impl Cursor {
    /// The results of the actions that ran. Every other action of the tree reads as
    /// null, so that a branch not taken can be tested with `absent`.
    pub(crate) fn steps(&self, actions: &[String]) -> Json {
        let mut steps = self.steps.clone();
        for action in actions {
            steps.entry(action.clone()).or_insert(Json::Null);
        }
        Json::Object(steps)
    }

    /// The item of each enclosing loop.
    pub(crate) fn items(&self) -> Map<String, Json> {
        let mut items = Map::new();
        for frame in &self.frames {
            if let Some(iteration) = &frame.iteration {
                let item = iteration.items.get(iteration.index).cloned();
                items.insert(iteration.name.clone(), item.unwrap_or(Json::Null));
            }
        }
        items
    }

    fn count(&mut self) -> Result<(), String> {
        self.count += 1;
        if self.count > MAX_STEPS {
            return Err(format!(
                "This triggering ran more than {MAX_STEPS} blocks and iterations."
            ));
        }
        Ok(())
    }

    /// Runs blocks until the next action, which it returns, or `None` once the tree is
    /// done. The cursor then stands after that action.
    pub(crate) fn advance<'a>(
        &mut self,
        steps: &'a [RuleNode],
        evaluator: &Evaluator,
    ) -> Result<Option<&'a RuleNode>, String> {
        loop {
            super::evaluate::check_deadline()?;
            let Some(frame) = self.frames.last_mut() else {
                return Ok(None);
            };
            let nodes = block(steps, &frame.path)
                .ok_or("This triggering's position no longer exists in its rule version.")?;
            if frame.next >= nodes.len() {
                if let Some(iteration) = &mut frame.iteration
                    && iteration.index + 1 < iteration.items.len()
                {
                    iteration.index += 1;
                    frame.next = 0;
                    self.count()?;
                    continue;
                }
                self.frames.pop();
                continue;
            }
            let position = frame.next;
            frame.next += 1;
            let path = frame.path.clone();
            let node = &nodes[position];
            self.count()?;
            match node {
                RuleNode::If { condition, .. } => {
                    let holds = self::condition(condition, evaluator, &self.items())?;
                    let branch = if holds {
                        Branch::Then(position)
                    } else {
                        Branch::Otherwise(position)
                    };
                    self.frames.push(Frame {
                        path: [path, vec![branch]].concat(),
                        next: 0,
                        iteration: None,
                    });
                }
                RuleNode::ForEach {
                    items, item_name, ..
                } => {
                    let items = match evaluator.value(items, &self.items())? {
                        Json::Array(items) => items,
                        Json::Null => Vec::new(),
                        _ => return Err(format!("{} does not hold a list.", node.id())),
                    };
                    if items.len() > MAX_LOOP_ITEMS {
                        return Err(format!(
                            "{} holds more than {MAX_LOOP_ITEMS} items.",
                            node.id()
                        ));
                    }
                    if !items.is_empty() {
                        self.frames.push(Frame {
                            path: [path, vec![Branch::Body(position)]].concat(),
                            next: 0,
                            iteration: Some(Iteration {
                                name: item_name.clone(),
                                items,
                                index: 0,
                            }),
                        });
                    }
                }
                RuleNode::Action { .. } => return Ok(Some(node)),
            }
        }
    }
}

/// A value, where a missing field, or a field of a missing value, reads as null.
fn lenient(
    value: &RuleValue,
    evaluator: &Evaluator,
    items: &Map<String, Json>,
) -> Result<Json, String> {
    match evaluator.value(value, items) {
        Err(error) if error.contains("No such key") || error.contains("(null_type, ") => {
            Ok(Json::Null)
        }
        result => result,
    }
}

fn number(value: &Json) -> Option<f64> {
    value.as_f64()
}

fn equal(left: &Json, right: &Json) -> bool {
    if let (Some(left), Some(right)) = (left.as_i64(), right.as_i64()) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (left.as_u64(), right.as_u64()) {
        return left == right;
    }
    if (left.is_i64() || left.is_u64()) && (right.is_i64() || right.is_u64()) {
        return false;
    }
    match (number(left), number(right)) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

fn text(value: &Json) -> Option<&str> {
    value.as_str()
}

fn compare(left: &Json, operator: RuleOperator, right: &Json) -> Result<bool, String> {
    Ok(match operator {
        RuleOperator::Equals => equal(left, right),
        RuleOperator::NotEquals => !equal(left, right),
        RuleOperator::Contains => match (left, right) {
            (Json::String(left), Json::String(right)) => left.contains(right.as_str()),
            (Json::Array(items), right) => items.iter().any(|item| equal(item, right)),
            (Json::Null, _) => false,
            _ => return Err("contains compares text with text, or a list with a value.".into()),
        },
        RuleOperator::StartsWith => text(left)
            .zip(text(right))
            .is_some_and(|(left, right)| left.starts_with(right)),
        RuleOperator::EndsWith => text(left)
            .zip(text(right))
            .is_some_and(|(left, right)| left.ends_with(right)),
        RuleOperator::Matches => {
            let pattern = text(right)
                .and_then(cairn_plugin_api::pattern)
                .ok_or("matches needs a valid regular expression.")?;
            text(left).is_some_and(|left| pattern.is_match(left))
        }
        RuleOperator::In => match right {
            Json::Array(items) => items.iter().any(|item| equal(left, item)),
            _ => return Err("is one of needs a list.".into()),
        },
        RuleOperator::Present => !left.is_null(),
        RuleOperator::Absent => left.is_null(),
        RuleOperator::GreaterThan | RuleOperator::LessThan => {
            let ordering = match (number(left), number(right), text(left), text(right)) {
                (Some(left), Some(right), _, _) => left.partial_cmp(&right),
                (_, _, Some(left), Some(right)) => Some(left.cmp(right)),
                _ if left.is_null() || right.is_null() => None,
                _ => return Err("Compare numbers with numbers, or texts with texts.".into()),
            };
            match operator {
                RuleOperator::GreaterThan => ordering == Some(std::cmp::Ordering::Greater),
                _ => ordering == Some(std::cmp::Ordering::Less),
            }
        }
    })
}

/// Evaluates a condition. Conditions about conversation keys belong to them.
pub(crate) fn condition(
    condition: &RuleCondition,
    evaluator: &Evaluator,
    items: &Map<String, Json>,
) -> Result<bool, String> {
    match condition {
        RuleCondition::All { conditions } => {
            for condition in conditions {
                if !self::condition(condition, evaluator, items)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        RuleCondition::Any { conditions } => {
            for condition in conditions {
                if self::condition(condition, evaluator, items)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        RuleCondition::Not { condition } => Ok(!self::condition(condition, evaluator, items)?),
        RuleCondition::Compare {
            left,
            operator,
            right,
        } => {
            let left = lenient(left, evaluator, items)?;
            let right = match right {
                Some(right) => lenient(right, evaluator, items)?,
                None => Json::Null,
            };
            compare(&left, *operator, &right)
        }
        RuleCondition::Formula { expression } => match evaluator.formula(expression, items)? {
            Json::Bool(holds) => Ok(holds),
            _ => Err("This formula does not evaluate to true or false.".into()),
        },
        RuleCondition::AuthorIn { author, people } => {
            let author = lenient(author, evaluator, items)?;
            Ok(author
                .as_str()
                .is_some_and(|author| people.iter().any(|person| person.id == author)))
        }
        RuleCondition::AuthorStartedConversation { .. }
        | RuleCondition::ConversationExists { .. }
        | RuleCondition::AuthorQuota { .. } => {
            Err("This condition needs conversation keys, which are not available yet.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::evaluate::Variables;
    use serde_json::json;

    fn evaluator(author: &str) -> Evaluator {
        let mut variables = Variables::default();
        variables.insert(
            "payload",
            json!({ "author": "alice", "items": ["a", "b"], "n": 3 }),
        );
        variables.insert("author", json!(author));
        variables.insert("steps", json!({ "post": null }));
        Evaluator::new(&variables).unwrap()
    }

    fn tree() -> Vec<RuleNode> {
        serde_json::from_value(json!([
            {
                "type": "if", "id": "known",
                "condition": { "type": "authorIn", "author": { "type": "formula", "expression": "author" },
                    "people": [{ "resourceType": "person", "id": "alice" }] },
                "then": [
                    { "type": "forEach", "id": "each", "itemName": "entry",
                      "items": { "type": "formula", "expression": "payload.items" },
                      "body": [{ "type": "action", "id": "post",
                        "action": { "type": "wait", "seconds": { "type": "formula", "expression": "size(entry)" } } }] },
                ],
                "otherwise": [{ "type": "action", "id": "refuse",
                    "action": { "type": "wait", "seconds": { "type": "literal", "value": 1 } } }],
            },
            { "type": "action", "id": "last", "action": { "type": "wait", "seconds": { "type": "literal", "value": 2 } } },
        ]))
        .unwrap()
    }

    #[test]
    fn walks_branches_and_loops_and_resumes_from_a_serialized_cursor() {
        let steps = tree();
        let base = evaluator("alice");
        let mut cursor = Cursor::default();
        let first = cursor.advance(&steps, &base).unwrap().unwrap();
        assert_eq!(first.id(), "post");
        assert_eq!(cursor.items()["entry"], json!("a"));
        cursor.steps.insert("post".into(), json!({ "ok": 1 }));

        // The position survives persistence between actions.
        let mut cursor: Cursor =
            serde_json::from_value(serde_json::to_value(&cursor).unwrap()).unwrap();
        let second = cursor.advance(&steps, &base).unwrap().unwrap();
        assert_eq!(second.id(), "post");
        assert_eq!(cursor.items()["entry"], json!("b"));
        assert_eq!(cursor.advance(&steps, &base).unwrap().unwrap().id(), "last");
        assert!(cursor.advance(&steps, &base).unwrap().is_none());

        let stranger = evaluator("mallory");
        let mut cursor = Cursor::default();
        assert_eq!(
            cursor.advance(&steps, &stranger).unwrap().unwrap().id(),
            "refuse"
        );
    }

    #[test]
    fn integer_comparisons_and_membership_do_not_round_large_ids() {
        let left = json!(1234567890123456789_i64);
        let right = json!(1234567890123456788_i64);
        assert!(!equal(&left, &right));
        assert!(!compare(&left, RuleOperator::In, &json!([1234567890123456788_u64])).unwrap());
        assert!(equal(&json!(u64::MAX), &json!(u64::MAX)));
        assert!(!equal(&json!(u64::MAX), &json!(-1)));
        assert!(equal(&json!(3), &json!(3.0)));
    }

    #[test]
    fn operators_compare_values_and_missing_fields_read_as_null() {
        let v = evaluator("alice");
        let cases = [
            (
                json!({ "type": "compare", "left": { "type": "formula", "expression": "payload.n" }, "operator": "greaterThan", "right": { "type": "literal", "value": 2.5 } }),
                true,
            ),
            (
                json!({ "type": "compare", "left": { "type": "formula", "expression": "payload.n" }, "operator": "equals", "right": { "type": "literal", "value": 3.0 } }),
                true,
            ),
            (
                json!({ "type": "compare", "left": { "type": "formula", "expression": "payload.missing" }, "operator": "absent" }),
                true,
            ),
            (
                json!({ "type": "compare", "left": { "type": "formula", "expression": "steps.post.messageId" }, "operator": "absent" }),
                true,
            ),
            (
                json!({ "type": "compare", "left": { "type": "formula", "expression": "payload.items" }, "operator": "contains", "right": { "type": "literal", "value": "b" } }),
                true,
            ),
            (
                json!({ "type": "compare", "left": { "type": "template", "text": "{{author}}!" }, "operator": "matches", "right": { "type": "literal", "value": "[a-z]+!" } }),
                true,
            ),
            (
                json!({ "type": "compare", "left": { "type": "literal", "value": "x" }, "operator": "in", "right": { "type": "literal", "value": ["y"] } }),
                false,
            ),
            (
                json!({ "type": "any", "conditions": [
                { "type": "formula", "expression": "false" },
                { "type": "not", "condition": { "type": "formula", "expression": "payload.n < 1" } },
            ] }),
                true,
            ),
        ];
        for (condition, expected) in cases {
            let parsed: RuleCondition = serde_json::from_value(condition.clone()).unwrap();
            assert_eq!(
                super::condition(&parsed, &v, &Map::new()),
                Ok(expected),
                "{condition}"
            );
        }
        let looping: Vec<RuleNode> = serde_json::from_value(json!([
            { "type": "forEach", "id": "each", "itemName": "entry",
              "items": { "type": "formula", "expression": "payload.n" }, "body": [] },
        ]))
        .unwrap();
        assert!(
            Cursor::default()
                .advance(&looping, &v)
                .unwrap_err()
                .contains("does not hold a list")
        );
    }
}
