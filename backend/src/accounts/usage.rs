//! Usage normalized across coding agents.
//!
//! `{"allowed":bool,"windows":[Window],"resets":{"available":n,"credits":[..]}|null}`, where a
//! window is `{"id","label","usedPercent","resetsAt","durationMins","models","reached"}`. A window
//! with no models limits every model; otherwise it only limits the models it names.
use serde_json::{Value, json};

pub fn empty() -> Value {
    json!({"allowed": true,"windows": [],"resets": null})
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

fn applies(window: &Value, model: &str) -> bool {
    window["models"]
        .as_array()
        .is_none_or(|models| models.is_empty() || models.iter().any(|m| m == model))
}

fn windows<'a>(usage: &'a Value, model: &'a str) -> impl Iterator<Item = &'a Value> {
    usage["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |w| applies(w, model))
}

fn left(window: &Value) -> Option<f64> {
    window["usedPercent"]
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| (100. - n).clamp(0., 100.))
}

/// The lowest remaining percentage among the windows limiting `model` ("" for any model).
pub fn remaining(usage: &Value, model: &str) -> Option<f64> {
    windows(usage, model).filter_map(left).reduce(f64::min)
}

/// The window that currently limits `model`, whose reset restores capacity first.
pub fn limiting<'a>(usage: &'a Value, model: &'a str) -> Option<&'a Value> {
    windows(usage, model)
        .filter(|w| left(w).is_some())
        .min_by(|a, b| left(a).unwrap().total_cmp(&left(b).unwrap()))
}

pub fn blocked(usage: &Value, model: &str) -> bool {
    usage["allowed"] == false || windows(usage, model).any(|w| w["reached"] == true)
}

/// Compare each window: one can reset while another still limits total capacity.
pub fn recovered(before: &Value, after: &Value, model: &str) -> bool {
    if blocked(after, model) || remaining(after, model).unwrap_or(0.) <= 0. {
        return false;
    }
    if before["allowed"] == false && after["allowed"] == true {
        return true;
    }
    windows(after, model).any(|current| {
        windows(before, model).any(|old| {
            old["id"] == current["id"]
                && matches!(
                    (old["usedPercent"].as_f64(), current["usedPercent"].as_f64()),
                    (Some(old), Some(now)) if now < old
                )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(general: f64, model: f64) -> Value {
        json!({
            "allowed": true,
            "windows": [
            {"id": "main:primary","usedPercent": general,"models": []},
            {"id": "main:secondary","usedPercent": 10.0,"models": []},
            {"id": "spark:primary","usedPercent": model,"models": ["gpt-spark"]},
        ]
        })
    }

    #[test]
    fn model_windows_only_limit_their_models() {
        assert_eq!(remaining(&usage(20., 95.), ""), Some(80.));
        assert_eq!(remaining(&usage(20., 95.), "gpt-spark"), Some(5.));
        assert_eq!(
            limiting(&usage(20., 95.), "gpt-spark").unwrap()["id"],
            "spark:primary"
        );
        assert_eq!(remaining(&empty(), ""), None);
        assert_eq!(remaining(&usage(130., 0.), ""), Some(0.));
    }

    #[test]
    fn blocked_and_recovered_consider_each_window() {
        let mut reached = usage(20., 40.);
        reached["windows"][2]["reached"] = true.into();
        assert!(blocked(&reached, "gpt-spark"));
        assert!(!blocked(&reached, ""));
        assert!(blocked(&json!({"allowed":false,"windows":[]}), ""));
        assert!(recovered(&usage(100., 0.), &usage(40., 0.), ""));
        assert!(!recovered(&usage(40., 0.), &usage(40., 0.), ""));
        assert!(!recovered(&usage(100., 0.), &usage(100., 0.), ""));
        let before = json!({"allowed": false,"windows": [{"id": "w","usedPercent": 10.0}]});
        assert!(recovered(&before, &usage(10., 0.), ""));
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
