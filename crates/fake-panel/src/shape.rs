//! Quirk-aware JSON value helpers.
//!
//! Without `mixed-types` every field has the type a typical Xtream UI panel uses. With it,
//! each (item, field) pair deterministically picks a variant: numbers as strings or ints,
//! string ids as ints, and some `null`s / empty strings for optional fields.

use serde_json::Value;

use crate::rng::{hash_str, mix3};

/// Value shaper for one response.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shape {
    mixed: bool,
    seed: u64,
}

impl Shape {
    pub(crate) fn new(mixed: bool, seed: u64) -> Self {
        Self { mixed, seed }
    }

    fn roll(&self, key: u64, field: &str) -> u64 {
        mix3(self.seed, key, hash_str(field)) % 100
    }

    /// Integer field (`num`, `stream_id`, `tv_archive`, …): int; mixed → 50 % string.
    pub(crate) fn int(&self, key: u64, field: &str, v: i64) -> Value {
        if self.mixed && self.roll(key, field) < 50 {
            Value::String(v.to_string())
        } else {
            Value::from(v)
        }
    }

    /// Numeric field panels send as a string (`added`, `category_id`, `exp_date`, …):
    /// string; mixed → 40 % int.
    pub(crate) fn numstr(&self, key: u64, field: &str, v: i64) -> Value {
        if self.mixed && self.roll(key, field) < 40 {
            Value::from(v)
        } else {
            Value::String(v.to_string())
        }
    }

    /// Optional text: string; mixed → 12 % `null`, 8 % `""`.
    pub(crate) fn text(&self, key: u64, field: &str, s: &str) -> Value {
        if self.mixed {
            match self.roll(key, field) {
                0..=11 => return Value::Null,
                12..=19 => return Value::String(String::new()),
                _ => {}
            }
        }
        Value::String(s.to_owned())
    }

    /// Float field (`rating_5based`): number; mixed → 40 % string.
    pub(crate) fn float(&self, key: u64, field: &str, v: f64) -> Value {
        if self.mixed && self.roll(key, field) < 40 {
            Value::String(fmt_num(v))
        } else {
            Value::from(v)
        }
    }

    /// Rating (`"7.4"` as a string); mixed → number, `""` or `null` sometimes.
    pub(crate) fn rating(&self, key: u64, field: &str, v: f64) -> Value {
        if self.mixed {
            match self.roll(key, field) {
                0..=29 => return Value::from(v),
                30..=37 => return Value::String(String::new()),
                38..=42 => return Value::Null,
                _ => {}
            }
        }
        Value::String(fmt_num(v))
    }

    /// Nulls `v` in mixed mode with probability `pct` %.
    pub(crate) fn nullable(&self, key: u64, field: &str, pct: u64, v: Value) -> Value {
        if self.mixed && self.roll(key, field) < pct {
            Value::Null
        } else {
            v
        }
    }
}

/// `7.0` → `"7"`, `7.4` → `"7.4"`.
pub(crate) fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// `HH:MM:SS`.
pub(crate) fn hms(secs: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}
