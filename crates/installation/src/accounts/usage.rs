//! Normalized usage from the existing coding-agent drivers.
pub use cairn_contracts::{
    AccountResets as Resets, AccountUsage as Usage, AccountWindow as Window,
};
use serde_json::Value;

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
    fn paid_balance_does_not_turn_an_unrelated_window_reset_into_recovery() {
        let mut before = usage(100., 50.);
        before["useBalance"] = true.into();
        before["windows"][0]["reached"] = true.into();
        let mut after = before.clone();
        after["windows"][2]["usedPercent"] = 0.into();
        assert_eq!(remaining(&after, "gpt-spark"), Some(0.));
        assert!(!recovered(&before, &after, "gpt-spark"));

        after["windows"][0]["usedPercent"] = 20.into();
        after["windows"][0]["reached"] = false.into();
        assert!(recovered(&before, &after, "gpt-spark"));
    }

    #[test]
    fn empty_usage_keeps_its_shape() {
        assert_eq!(
            empty(),
            json!({ "allowed": true, "windows": [], "resets": null, "useBalance": false })
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
