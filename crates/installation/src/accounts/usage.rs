//! Usage normalized across coding agents.
//!
//! `{"allowed":bool,"windows":[Window],"resets":{"available":n,"credits":[..]}|null}`, where a
//! window is `{"id","label","usedPercent","resetsAt","durationMins","models","reached"}`. A window
//! with no models limits every model; otherwise it only limits the models it names.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default = "allowed")]
    pub allowed: bool,
    #[serde(default)]
    pub windows: Vec<Window>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    /// Claude Code's last reading attempt, successful or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted_at: Option<i64>,
    /// Codex banked resets.
    #[serde(default)]
    pub resets: Option<Resets>,
    /// Kept as written: Claude Code sets `error` to a message or `null`, Codex never sets it.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn allowed() -> bool {
    true
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            allowed: true,
            windows: Vec::new(),
            checked_at: None,
            attempted_at: None,
            resets: None,
            extra: Map::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// Kept as reported, so integer readings stay integers.
    #[serde(default)]
    pub used_percent: Option<Number>,
    #[serde(default)]
    pub resets_at: Option<i64>,
    #[serde(default)]
    pub duration_mins: Option<i64>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub reached: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resets {
    #[serde(default)]
    pub available: u64,
    /// Codex's own credit objects, kept verbatim.
    #[serde(default)]
    pub credits: Vec<Value>,
}

impl Window {
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

impl Usage {
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
        self.extra.insert("error".into(), error.into());
    }

    pub fn available_resets(&self) -> u64 {
        self.resets.as_ref().map_or(0, |r| r.available)
    }

    fn windows<'a>(&'a self, model: &'a str) -> impl Iterator<Item = &'a Window> {
        self.windows.iter().filter(move |w| w.applies(model))
    }

    /// The lowest remaining percentage among the windows limiting `model` ("" for any model).
    pub fn remaining(&self, model: &str) -> Option<f64> {
        self.windows(model)
            .filter_map(Window::left)
            .reduce(f64::min)
    }

    /// The window that currently limits `model`, whose reset restores capacity first.
    pub fn limiting<'a>(&'a self, model: &'a str) -> Option<&'a Window> {
        self.windows(model)
            .filter_map(|w| w.left().map(|left| (w, left)))
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(w, _)| w)
    }

    pub fn blocked(&self, model: &str) -> bool {
        !self.allowed || self.windows(model).any(|w| w.reached)
    }

    /// Unblocked, with capacity left for `model`.
    pub fn available(&self, model: &str) -> bool {
        !self.blocked(model) && self.remaining(model).unwrap_or(0.) > 0.
    }

    /// Compare each window: one can reset while another still limits total capacity.
    pub fn recovered(before: &Self, after: &Self, model: &str) -> bool {
        if !after.available(model) {
            return false;
        }
        if !before.allowed && after.allowed {
            return true;
        }
        after.windows(model).any(|current| {
            before.windows(model).any(|old| {
                old.id == current.id
                    && matches!((old.used(), current.used()), (Some(old), Some(now)) if now < old)
            })
        })
    }
}

pub fn empty() -> Value {
    Usage::default().to_value()
}

pub fn duration_label(minutes: Option<i64>) -> String {
    match minutes {
        None | Some(0) => "Usage window".into(),
        Some(10080) => "Weekly".into(),
        Some(m) if m % 1440 == 0 => format!("{}-day window", m / 1440),
        Some(m) if m % 60 == 0 => format!("{}-hour window", m / 60),
        Some(m) => format!("{m}-minute window"),
    }
}

/// The lowest remaining percentage among the windows limiting `model` ("" for any model).
pub fn remaining(usage: &Value, model: &str) -> Option<f64> {
    Usage::from_value(usage).remaining(model)
}

pub fn blocked(usage: &Value, model: &str) -> bool {
    Usage::from_value(usage).blocked(model)
}

/// Compare each window: one can reset while another still limits total capacity.
pub fn recovered(before: &Value, after: &Value, model: &str) -> bool {
    Usage::recovered(&Usage::from_value(before), &Usage::from_value(after), model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(general: f64, model: f64) -> Value {
        json!({
            "allowed": true,
            "windows": [
                { "id": "main:primary", "usedPercent": general, "models": [] },
                { "id": "main:secondary", "usedPercent": 10.0, "models": [] },
                { "id": "spark:primary", "usedPercent": model, "models": ["gpt-spark"] },
            ],
        })
    }

    #[test]
    fn model_windows_only_limit_their_models() {
        assert_eq!(remaining(&usage(20., 95.), ""), Some(80.));
        assert_eq!(remaining(&usage(20., 95.), "gpt-spark"), Some(5.));
        let limiting = Usage::from_value(&usage(20., 95.));
        assert_eq!(limiting.limiting("gpt-spark").unwrap().id, "spark:primary");
        assert_eq!(remaining(&empty(), ""), None);
        assert_eq!(remaining(&usage(130., 0.), ""), Some(0.));
    }

    #[test]
    fn blocked_and_recovered_consider_each_window() {
        let mut reached = usage(20., 40.);
        reached["windows"][2]["reached"] = true.into();
        assert!(blocked(&reached, "gpt-spark"));
        assert!(!blocked(&reached, ""));
        assert!(blocked(&json!({ "allowed": false, "windows": [] }), ""));
        assert!(recovered(&usage(100., 0.), &usage(40., 0.), ""));
        assert!(!recovered(&usage(40., 0.), &usage(40., 0.), ""));
        assert!(!recovered(&usage(100., 0.), &usage(100., 0.), ""));
        let before = json!({ "allowed": false, "windows": [{ "id": "w", "usedPercent": 10.0 }] });
        assert!(recovered(&before, &usage(10., 0.), ""));
    }

    #[test]
    fn empty_usage_keeps_its_shape() {
        assert_eq!(
            empty(),
            json!({ "allowed": true, "windows": [], "resets": null })
        );
    }

    #[test]
    fn durations_have_readable_labels() {
        assert_eq!(duration_label(Some(300)), "5-hour window");
        assert_eq!(duration_label(Some(10080)), "Weekly");
        assert_eq!(duration_label(Some(2880)), "2-day window");
        assert_eq!(duration_label(Some(45)), "45-minute window");
        assert_eq!(duration_label(None), "Usage window");
    }
}
