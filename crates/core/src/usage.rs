//! Usage analytics from `GET /api/analytics/usage?days=&profile=` (source
//! `hermes_cli/web_routers/analytics.py`): one profile's session store, summed per local day and
//! per model. "Requests" are Hermes' `api_calls` (model API calls).

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub reasoning_tokens: i64,
    /// Hermes' price-table estimate, in USD.
    pub estimated_cost: f64,
    /// What the provider reported, in USD (0 when it reports nothing).
    pub actual_cost: f64,
    pub sessions: i64,
    pub requests: i64,
    /// For sums over several bots: each part's best figure added up, so one bot's provider
    /// cost doesn't hide the others' estimates.
    pub combined_cost: Option<f64>,
}

impl Totals {
    pub fn tokens(&self) -> i64 {
        self.input_tokens + self.output_tokens
    }
    /// The provider's figure when it gave one, else the estimate.
    pub fn cost(&self) -> f64 {
        if let Some(c) = self.combined_cost {
            return c;
        }
        if self.actual_cost > 0.0 {
            self.actual_cost
        } else {
            self.estimated_cost
        }
    }
    pub fn add(&mut self, o: &Totals) {
        self.combined_cost = Some(self.cost() + o.cost());
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.reasoning_tokens += o.reasoning_tokens;
        self.estimated_cost += o.estimated_cost;
        self.actual_cost += o.actual_cost;
        self.sessions += o.sessions;
        self.requests += o.requests;
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Day {
    /// `YYYY-MM-DD`, local calendar day.
    pub day: String,
    pub t: Totals,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelRow {
    pub model: String,
    pub t: Totals,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub days: u32,
    pub totals: Totals,
    pub daily: Vec<Day>,
    pub by_model: Vec<ModelRow>,
}

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))).unwrap_or(0)
}
fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn row(v: &Value) -> Totals {
    Totals {
        input_tokens: i(v, "input_tokens"),
        output_tokens: i(v, "output_tokens"),
        cache_read_tokens: i(v, "cache_read_tokens"),
        reasoning_tokens: i(v, "reasoning_tokens"),
        estimated_cost: f(v, "estimated_cost"),
        actual_cost: f(v, "actual_cost"),
        sessions: i(v, "sessions"),
        requests: i(v, "api_calls"),
        combined_cost: None,
    }
}

pub fn parse(v: &Value) -> Usage {
    let t = v.get("totals").cloned().unwrap_or(Value::Null);
    Usage {
        days: v.get("period_days").and_then(Value::as_u64).unwrap_or(30) as u32,
        totals: Totals {
            input_tokens: i(&t, "total_input"),
            output_tokens: i(&t, "total_output"),
            cache_read_tokens: i(&t, "total_cache_read"),
            reasoning_tokens: i(&t, "total_reasoning"),
            estimated_cost: f(&t, "total_estimated_cost"),
            actual_cost: f(&t, "total_actual_cost"),
            sessions: i(&t, "total_sessions"),
            requests: i(&t, "total_api_calls"),
            combined_cost: None,
        },
        daily: v.get("daily").and_then(Value::as_array).into_iter().flatten().map(|d| Day { day: d.get("day").and_then(Value::as_str).unwrap_or_default().to_owned(), t: row(d) }).collect(),
        by_model: v.get("by_model").and_then(Value::as_array).into_iter().flatten().map(|d| ModelRow { model: d.get("model").and_then(Value::as_str).unwrap_or("unknown").to_owned(), t: row(d) }).collect(),
    }
}

/// Sum several profiles' daily rows into one series (the "All bots" view).
pub fn merge_daily(parts: &[&Usage]) -> Vec<Day> {
    let mut map: std::collections::BTreeMap<String, Totals> = Default::default();
    for u in parts {
        for d in &u.daily {
            map.entry(d.day.clone()).or_default().add(&d.t);
        }
    }
    map.into_iter().map(|(day, t)| Day { day, t }).collect()
}

/// 1234 → "1.2K", 3_400_000 → "3.4M".
pub fn fmt_count(n: i64) -> String {
    let a = n.abs() as f64;
    if a >= 1e9 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if a >= 1e6 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if a >= 1e3 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

pub fn fmt_cost(usd: f64) -> String {
    if usd == 0.0 {
        "$0".into()
    } else if usd < 0.01 {
        "<$0.01".into()
    } else {
        format!("${usd:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_bots_provider_cost_does_not_hide_others_estimates() {
        let a = Totals { actual_cost: 1.0, estimated_cost: 0.8, ..Default::default() };
        let b = Totals { estimated_cost: 5.0, ..Default::default() };
        let mut sum = Totals::default();
        sum.add(&a);
        sum.add(&b);
        assert!((sum.cost() - 6.0).abs() < 1e-9, "{}", sum.cost());
    }
    use serde_json::json;

    #[test]
    fn parses_analytics() {
        let v = json!({"period_days": 7, "daily": [{"day": "2026-11-15", "input_tokens": 1000, "output_tokens": 250, "estimated_cost": 0.5, "actual_cost": 0, "sessions": 2, "api_calls": 9}],
            "by_model": [{"model": "m", "input_tokens": 1000, "output_tokens": 250, "estimated_cost": 0.5, "sessions": 2, "api_calls": 9}],
            "totals": {"total_input": 1000, "total_output": 250, "total_estimated_cost": 0.5, "total_actual_cost": 0, "total_sessions": 2, "total_api_calls": 9}});
        let u = parse(&v);
        assert_eq!(u.totals.tokens(), 1250);
        assert_eq!(u.totals.requests, 9);
        assert_eq!(u.totals.cost(), 0.5);
        assert_eq!(u.daily[0].t.sessions, 2);
        let m = merge_daily(&[&u, &u]);
        assert_eq!(m[0].t.requests, 18);
        assert_eq!(fmt_count(1250), "1.2K");
        assert_eq!(fmt_cost(0.004), "<$0.01");
    }
}
