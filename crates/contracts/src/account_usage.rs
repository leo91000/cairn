//! AccountUsage normalized across coding agents.
//!
//! `{"allowed":bool,"windows":[AccountWindow],"resets":{"available":n,"credits":[..]}|null}`, where a
//! window is `{"id","label","usedPercent","resetsAt","durationMins","models","reached"}`. A window
//! with no models limits every model; otherwise it only limits the models it names.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    #[serde(default = "allowed")]
    pub allowed: bool,
    #[serde(default)]
    pub windows: Vec<AccountWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub checked_at: Option<i64>,
    /// The last reading attempt, successful or not (Claude Code and OpenCode Go).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub attempted_at: Option<i64>,
    /// Codex banked resets.
    #[serde(default)]
    pub resets: Option<AccountResets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub use_balance: bool,
}

fn allowed() -> bool {
    true
}

impl Default for AccountUsage {
    fn default() -> Self {
        Self {
            allowed: true,
            windows: Vec::new(),
            checked_at: None,
            attempted_at: None,
            resets: None,
            error: None,
            use_balance: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountWindow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// Kept as reported, so integer readings stay integers.
    #[serde(default)]
    #[schemars(with = "Option<f64>")]
    #[ts(type = "number | null")]
    pub used_percent: Option<Number>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub duration_mins: Option<i64>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub reached: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccountResets {
    #[serde(default)]
    #[ts(type = "number")]
    pub available: u64,
    /// Codex's own credit objects, kept verbatim.
    #[serde(default)]
    #[schemars(with = "Vec<crate::PluginValues>")]
    #[ts(type = "Array<Record<string, unknown>>")]
    pub credits: Vec<Value>,
}

impl AccountWindow {
    fn applies(&self, model: &str) -> bool {
        self.models.is_empty() || self.models.iter().any(|m| m == model)
    }

    fn used(&self) -> Option<f64> {
        self.used_percent.as_ref().and_then(Number::as_f64)
    }

    fn left(&self) -> Option<f64> {
        self.used()
            .filter(|n| n.is_finite())
            .map(|n| (100. - n).clamp(0., 100.))
    }
}

impl AccountUsage {
    /// Reads usage leniently: anything unreadable counts as no usage known.
    pub fn from_value(value: &Value) -> Self {
        Self::parse(value).unwrap_or_default()
    }

    /// `None` for `null` or an unreadable value.
    pub fn parse(value: &Value) -> Option<Self> {
        if value.is_null() {
            return None;
        }
        Self::deserialize(value).ok()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn set_error(&mut self, error: Option<&str>) {
        self.error = error.map(str::to_owned);
    }

    pub fn available_resets(&self) -> u64 {
        self.resets.as_ref().map_or(0, |r| r.available)
    }

    /// OpenCode Go can continue on Zen credits when the user enabled it at the provider.
    pub fn uses_balance(&self) -> bool {
        self.use_balance
    }

    /// Whether a window is scoped to `model` by name.
    pub fn names(&self, model: &str) -> bool {
        self.windows
            .iter()
            .any(|w| w.models.iter().any(|m| m == model))
    }

    fn windows<'a>(&'a self, model: &'a str) -> impl Iterator<Item = &'a AccountWindow> {
        self.windows.iter().filter(move |w| w.applies(model))
    }

    /// The lowest remaining percentage among the windows limiting `model` ("" for any model).
    pub fn remaining(&self, model: &str) -> Option<f64> {
        self.windows(model)
            .filter_map(AccountWindow::left)
            .reduce(f64::min)
    }

    /// The window that currently limits `model`, whose reset restores capacity first.
    pub fn limiting<'a>(&'a self, model: &'a str) -> Option<&'a AccountWindow> {
        self.windows(model)
            .filter_map(|w| w.left().map(|left| (w, left)))
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(w, _)| w)
    }

    pub fn blocked(&self, model: &str) -> bool {
        !self.allowed || (!self.uses_balance() && self.windows(model).any(|w| w.reached))
    }

    /// Unblocked, with capacity left for `model`.
    pub fn available(&self, model: &str) -> bool {
        !self.blocked(model) && (self.uses_balance() || self.remaining(model).unwrap_or(0.) > 0.)
    }

    /// Compare each window: one can reset while another still limits total capacity.
    pub fn recovered(before: &Self, after: &Self, model: &str) -> bool {
        if !after.available(model) {
            return false;
        }
        if (!before.allowed && after.allowed) || (!before.uses_balance() && after.uses_balance()) {
            return true;
        }
        // A decrease proves quota recovery only when every limiting window has
        // capacity. The provider's paid balance is not measured by these windows.
        let window_capacity = !after.windows(model).any(|window| window.reached)
            && after.remaining(model).is_some_and(|left| left > 0.);
        window_capacity
            && after.windows(model).any(|current| {
                before.windows(model).any(|old| {
                old.id == current.id
                    && matches!((old.used(), current.used()), (Some(old), Some(now)) if now < old)
            })
            })
    }
}

fn is_false(value: &bool) -> bool {
    !value
}
