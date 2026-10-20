// SPDX-License-Identifier: GPL-3.0-or-later
//! Lenient `serde` helpers for the loosely-typed Xtream Codes JSON, and the resilient
//! list parser.
//!
//! Xtream panels are PHP applications with decades of forks, so the same field can be
//! any of: a number, a numeric string, `""`, `null`, `"N/A"`, an array where an object
//! was expected (`[]` is PHP's empty associative array), and so on. The helpers here
//! are meant for `#[serde(default, deserialize_with = "…")]` and **never fail on a
//! wrong type** — they fall back to `None` / empty / `false` instead. Only the
//! `required_*` helpers fail, which makes the *item* invalid (see below).
//!
//! | Helper | Accepts | Result |
//! |---|---|---|
//! | [`string`], [`opt_string`] | string (trimmed), number, bool | `""`/whitespace/`null`/objects → empty / `None` |
//! | [`opt_u64`], [`opt_u32`], [`opt_u16`], [`opt_i64`] | `5`, `"5"`, `" 5 "`, `5.0`, `"5.0"` | anything else → `None` |
//! | [`opt_f64`] | `7.5`, `"7.5"`, `"7,5"` | `""`, `"N/A"`, non-finite → `None` |
//! | [`bool()`] | `true`, `1`, `"1"`, `"true"`, `"yes"` | `0`, `"0"`, `""`, `null`, other → `false` |
//! | [`string_list`] | `"a"`, `["a","b"]`, `[1,2]` | `null`, `""`, objects → `[]` |
//! | [`opt_timestamp`] | epoch seconds as number or string | `0`, negative, `""`, `null` → `None` |
//! | [`object_or_default`], [`object_or_none`] | an object | `[]`, `null`, `""`, a malformed object → default / `None` |
//! | [`lenient_vec`] | an array, or an object keyed by index | malformed elements are dropped |
//!
//! ## Lists
//!
//! [`parse_list`] parses a top-level list endpoint (`get_live_streams`, …). One malformed
//! item is **skipped and reported** in [`Listing::skipped`] / [`Listing::errors`]; it never
//! fails the whole list. It also accepts the PHP object-keyed-by-index form
//! (`{"0":{…},"1":{…}}`), treats an empty body / `null` / `false` / `""` as an empty list,
//! and maps an auth-failure object (`{"user_info":{"auth":0}}`) returned in place of the
//! array to [`Error::Auth`].

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

use crate::error::{AuthFailure, Error};

/// Maximum number of per-item errors kept in [`Listing::errors`] (the count in
/// [`Listing::skipped`] is always exact).
pub const MAX_ITEM_ERRORS: usize = 16;

/// Maximum length of a single [`ItemError::message`], in bytes.
const MAX_ERROR_MESSAGE: usize = 200;

// ---------------------------------------------------------------------------------------
// Scalar plumbing
// ---------------------------------------------------------------------------------------

/// Any JSON value, reduced to what the lenient helpers care about.
#[derive(Debug, Clone, PartialEq)]
enum Scalar {
    Str(String),
    Int(i64),
    UInt(u64),
    Float(f64),
    Bool(bool),
    Null,
    /// An array or an object (already consumed and ignored).
    Compound,
}

struct ScalarVisitor;

impl<'de> Visitor<'de> for ScalarVisitor {
    type Value = Scalar;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Scalar, E> {
        Ok(Scalar::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Scalar, E> {
        Ok(Scalar::Int(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Scalar, E> {
        Ok(Scalar::UInt(v))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Scalar, E> {
        Ok(Scalar::Float(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Scalar, E> {
        Ok(Scalar::Str(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Scalar, E> {
        Ok(Scalar::Str(v))
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Scalar, E> {
        Ok(Scalar::Str(String::from_utf8_lossy(v).into_owned()))
    }

    fn visit_none<E>(self) -> Result<Scalar, E> {
        Ok(Scalar::Null)
    }

    fn visit_unit<E>(self) -> Result<Scalar, E> {
        Ok(Scalar::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Scalar, D::Error> {
        d.deserialize_any(ScalarVisitor)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Scalar, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Scalar::Compound)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scalar, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Scalar::Compound)
    }
}

fn scalar<'de, D: Deserializer<'de>>(d: D) -> Result<Scalar, D::Error> {
    d.deserialize_any(ScalarVisitor)
}

/// Formats a float without a spurious `.0` when it is integral.
fn float_to_string(f: f64) -> Option<String> {
    if !f.is_finite() {
        return None;
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        // Exact: |f| < 2^53, integral.
        #[allow(clippy::cast_possible_truncation)]
        let i = f as i64;
        Some(i.to_string())
    } else {
        Some(f.to_string())
    }
}

fn float_to_u64(f: f64) -> Option<u64> {
    if f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f < 1.8e19 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(f as u64)
    } else {
        None
    }
}

fn float_to_i64(f: f64) -> Option<i64> {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.2e18 {
        #[allow(clippy::cast_possible_truncation)]
        Some(f as i64)
    } else {
        None
    }
}

/// Parses a lenient decimal: trims, accepts `,` as decimal separator, rejects non-finite.
pub(crate) fn parse_f64_str(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.len() > 64 {
        return None;
    }
    let parsed = match t.parse::<f64>() {
        Ok(v) => v,
        Err(_) => t.replace(',', ".").parse::<f64>().ok()?,
    };
    // Reject spellings like "inf"/"NaN" that Rust's parser accepts.
    parsed.is_finite().then_some(parsed)
}

/// Parses a lenient unsigned integer: `"5"`, `" 5 "`, `"+5"`, `"5.0"`.
pub(crate) fn parse_u64_str(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<u64>()
        .ok()
        .or_else(|| parse_f64_str(t).and_then(float_to_u64))
}

/// Parses a lenient signed integer.
pub(crate) fn parse_i64_str(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<i64>()
        .ok()
        .or_else(|| parse_f64_str(t).and_then(float_to_i64))
}

/// Parses a lenient boolean string.
pub(crate) fn parse_bool_str(s: &str) -> bool {
    let t = s.trim();
    if t.eq_ignore_ascii_case("true")
        || t.eq_ignore_ascii_case("yes")
        || t.eq_ignore_ascii_case("on")
    {
        return true;
    }
    parse_f64_str(t).is_some_and(|v| v != 0.0)
}

impl Scalar {
    fn into_opt_string(self) -> Option<String> {
        match self {
            Scalar::Str(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else if trimmed.len() == s.len() {
                    Some(s)
                } else {
                    Some(trimmed.to_owned())
                }
            }
            Scalar::Int(i) => Some(i.to_string()),
            Scalar::UInt(u) => Some(u.to_string()),
            Scalar::Float(f) => float_to_string(f),
            Scalar::Bool(b) => Some(if b { "true" } else { "false" }.to_owned()),
            Scalar::Null | Scalar::Compound => None,
        }
    }

    fn to_u64(&self) -> Option<u64> {
        match self {
            Scalar::Str(s) => parse_u64_str(s),
            Scalar::Int(i) => u64::try_from(*i).ok(),
            Scalar::UInt(u) => Some(*u),
            Scalar::Float(f) => float_to_u64(*f),
            Scalar::Bool(_) | Scalar::Null | Scalar::Compound => None,
        }
    }

    fn to_i64(&self) -> Option<i64> {
        match self {
            Scalar::Str(s) => parse_i64_str(s),
            Scalar::Int(i) => Some(*i),
            Scalar::UInt(u) => i64::try_from(*u).ok(),
            Scalar::Float(f) => float_to_i64(*f),
            Scalar::Bool(_) | Scalar::Null | Scalar::Compound => None,
        }
    }

    fn to_f64(&self) -> Option<f64> {
        match self {
            Scalar::Str(s) => parse_f64_str(s),
            #[allow(clippy::cast_precision_loss)]
            Scalar::Int(i) => Some(*i as f64),
            #[allow(clippy::cast_precision_loss)]
            Scalar::UInt(u) => Some(*u as f64),
            Scalar::Float(f) => f.is_finite().then_some(*f),
            Scalar::Bool(_) | Scalar::Null | Scalar::Compound => None,
        }
    }

    fn to_bool(&self) -> bool {
        match self {
            Scalar::Str(s) => parse_bool_str(s),
            Scalar::Int(i) => *i != 0,
            Scalar::UInt(u) => *u != 0,
            Scalar::Float(f) => *f != 0.0 && f.is_finite(),
            Scalar::Bool(b) => *b,
            Scalar::Null | Scalar::Compound => false,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Public field helpers
// ---------------------------------------------------------------------------------------

/// A string (trimmed); numbers and bools are stringified; anything else is `""`.
pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(scalar(d)?.into_opt_string().unwrap_or_default())
}

/// A non-empty trimmed string, or `None`.
pub fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(scalar(d)?.into_opt_string())
}

/// A non-negative integer from a number or numeric string, or `None`.
pub fn opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(scalar(d)?.to_u64())
}

/// Like [`opt_u64`], `None` when out of range for `u32`.
pub fn opt_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(scalar(d)?.to_u64().and_then(|v| u32::try_from(v).ok()))
}

/// A port-like value: like [`opt_u64`], `None` when `0` or out of range for `u16`.
pub fn opt_u16<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    Ok(scalar(d)?
        .to_u64()
        .and_then(|v| u16::try_from(v).ok())
        .filter(|v| *v != 0))
}

/// A signed integer from a number or numeric string, or `None`.
pub fn opt_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(scalar(d)?.to_i64())
}

/// A finite float from a number or string (`"7.5"`, `"7,5"`); `""`, `"N/A"` → `None`.
pub fn opt_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(scalar(d)?.to_f64())
}

/// A lenient boolean: `true`/`1`/`"1"`/`"true"`/`"yes"` are true, everything else false.
pub fn bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(scalar(d)?.to_bool())
}

/// A required non-negative integer id. Fails (making the enclosing item invalid) when
/// the value is missing or not numeric.
pub fn required_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    scalar(d)?
        .to_u64()
        .ok_or_else(|| serde::de::Error::custom("expected a numeric id"))
}

/// A required non-empty string id (numbers are stringified).
pub fn required_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    scalar(d)?
        .into_opt_string()
        .ok_or_else(|| serde::de::Error::custom("expected a non-empty id"))
}

/// Epoch seconds (number or string) → UTC time. `0`, negatives, `""` and `null` → `None`.
pub fn opt_timestamp<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
    Ok(scalar(d)?.to_i64().and_then(timestamp))
}

pub(crate) fn timestamp(secs: i64) -> Option<DateTime<Utc>> {
    if secs <= 0 {
        None
    } else {
        DateTime::from_timestamp(secs, 0)
    }
}

/// A string or an array of strings (`backdrop_path`, `allowed_output_formats`,
/// `category_ids`). Empty strings are dropped; numbers are stringified.
pub fn string_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    struct ListVisitor;

    impl<'de> Visitor<'de> for ListVisitor {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a string or a list of strings")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<String>, A::Error> {
            let mut out = Vec::new();
            while let Some(LenientString(item)) = seq.next_element::<LenientString>()? {
                if let Some(s) = item {
                    out.push(s);
                }
            }
            Ok(out)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Vec<String>, A::Error> {
            // PHP object keyed by index: keep the values.
            let mut out = Vec::new();
            while let Some((_, LenientString(item))) =
                map.next_entry::<IgnoredAny, LenientString>()?
            {
                if let Some(s) = item {
                    out.push(s);
                }
            }
            Ok(out)
        }

        fn visit_str<E>(self, v: &str) -> Result<Vec<String>, E> {
            Ok(Scalar::Str(v.to_owned())
                .into_opt_string()
                .into_iter()
                .collect())
        }

        fn visit_i64<E>(self, v: i64) -> Result<Vec<String>, E> {
            Ok(vec![v.to_string()])
        }

        fn visit_u64<E>(self, v: u64) -> Result<Vec<String>, E> {
            Ok(vec![v.to_string()])
        }

        fn visit_f64<E>(self, v: f64) -> Result<Vec<String>, E> {
            Ok(float_to_string(v).into_iter().collect())
        }

        fn visit_bool<E>(self, _: bool) -> Result<Vec<String>, E> {
            Ok(Vec::new())
        }

        fn visit_none<E>(self) -> Result<Vec<String>, E> {
            Ok(Vec::new())
        }

        fn visit_unit<E>(self) -> Result<Vec<String>, E> {
            Ok(Vec::new())
        }

        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Vec<String>, D::Error> {
            d.deserialize_any(ListVisitor)
        }
    }

    d.deserialize_any(ListVisitor)
}

/// Deserialises an object into `T`; a non-object (`[]`, `null`, `""`) or an object that
/// does not fit `T` yields `T::default()`.
pub fn object_or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    Ok(object_or_none(d)?.unwrap_or_default())
}

/// Like [`object_or_default`], but yields `None` instead of a default.
pub fn object_or_none<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(d)?;
    if value.is_object() {
        Ok(T::deserialize(value).ok())
    } else {
        Ok(None)
    }
}

/// An array (or an object keyed by index) of `T`, dropping elements that fail to
/// deserialise. Anything else yields an empty vector.
pub fn lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(lenient_items(value)
        .filter_map(|v| T::deserialize(v).ok())
        .collect())
}

/// The element values of an array, or the values of an object keyed by index.
pub(crate) fn lenient_items(
    value: serde_json::Value,
) -> Box<dyn Iterator<Item = serde_json::Value>> {
    match value {
        serde_json::Value::Array(items) => Box::new(items.into_iter()),
        serde_json::Value::Object(map) => Box::new(map.into_iter().map(|(_, v)| v)),
        _ => Box::new(std::iter::empty()),
    }
}

/// A string that deserialises leniently from any JSON value (see [`opt_string`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LenientString(pub Option<String>);

impl<'de> Deserialize<'de> for LenientString {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        opt_string(d).map(LenientString)
    }
}

// ---------------------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------------------

/// The result of parsing a list endpoint: the valid items plus a report of the skipped
/// ones.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing<T> {
    /// Items that parsed, in server order.
    pub items: Vec<T>,
    /// Exact number of items that were skipped because they were malformed.
    pub skipped: usize,
    /// Details for the first [`MAX_ITEM_ERRORS`] skipped items.
    pub errors: Vec<ItemError>,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Listing {
            items: Vec::new(),
            skipped: 0,
            errors: Vec::new(),
        }
    }
}

impl<T> Listing<T> {
    /// Number of valid items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` when there are no valid items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Records a skipped item.
    pub(crate) fn skip(&mut self, index: usize, message: impl Into<String>) {
        self.skipped += 1;
        if self.errors.len() < MAX_ITEM_ERRORS {
            let mut message = message.into();
            truncate_at_char_boundary(&mut message, MAX_ERROR_MESSAGE);
            self.errors.push(ItemError { index, message });
        }
    }

    /// Maps the items, keeping the skip report.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Listing<U> {
        Listing {
            items: self.items.into_iter().map(f).collect(),
            skipped: self.skipped,
            errors: self.errors,
        }
    }
}

/// Why one list item was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemError {
    /// Zero-based position of the item in the server's list.
    pub index: usize,
    /// What was wrong with it (from the deserialiser; never the whole item).
    pub message: String,
}

pub(crate) fn truncate_at_char_boundary(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

/// Decodes a response body: strips a UTF-8 BOM and replaces invalid UTF-8 sequences.
pub(crate) fn body_text(body: &[u8]) -> Cow<'_, str> {
    let body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    String::from_utf8_lossy(body)
}

/// Parses a top-level JSON list of `T` resiliently. See the [module docs](self).
///
/// # Errors
///
/// [`Error::Parse`] when the body is not JSON or not list-like, [`Error::Auth`] when it
/// is an auth-failure object.
pub fn parse_list<T: DeserializeOwned>(body: &[u8]) -> Result<Listing<T>, Error> {
    parse_list_with(body, |raw| {
        serde_json::from_str::<T>(raw.get()).map_err(|e| e.to_string())
    })
}

/// Like [`parse_list`] with a custom per-item converter.
pub(crate) fn parse_list_with<T>(
    body: &[u8],
    convert: impl FnMut(&RawValue) -> Result<T, String>,
) -> Result<Listing<T>, Error> {
    let text = body_text(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Listing::default());
    }
    let top: &RawValue = serde_json::from_str(trimmed).map_err(|e| not_json(&e))?;
    list_from_raw(top, convert)
}

pub(crate) fn not_json(e: &serde_json::Error) -> Error {
    // serde_json messages contain positions, not content.
    Error::parse(format!("the response is not valid JSON ({e})"))
}

/// An object's entries in document order, values left unparsed.
pub(crate) struct OrderedEntries<'a>(pub(crate) Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for OrderedEntries<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct EntriesVisitor<'a>(PhantomData<&'a ()>);

        impl<'de> Visitor<'de> for EntriesVisitor<'de> {
            type Value = OrderedEntries<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, &'de RawValue>()? {
                    entries.push((k, v));
                }
                Ok(OrderedEntries(entries))
            }
        }

        d.deserialize_map(EntriesVisitor(PhantomData))
    }
}

#[derive(Deserialize)]
struct AuthProbe {
    #[serde(default, deserialize_with = "bool")]
    auth: bool,
}

/// If `entries` is a `{"user_info": {...}}` object, returns the matching error.
pub(crate) fn auth_object_error(entries: &[(String, &RawValue)]) -> Option<Error> {
    let (_, user_info) = entries.iter().find(|(k, _)| k == "user_info")?;
    let auth = serde_json::from_str::<AuthProbe>(user_info.get())
        .map(|p| p.auth)
        .unwrap_or(false);
    Some(if auth {
        Error::parse(
            "the server answered with account information instead of the requested data (unsupported action?)",
        )
    } else {
        Error::Auth {
            status: AuthFailure::InvalidCredentials,
        }
    })
}

/// Parses a raw JSON value that should be a list (array, object keyed by index, or an
/// empty-ish scalar).
pub(crate) fn list_from_raw<T>(
    top: &RawValue,
    mut convert: impl FnMut(&RawValue) -> Result<T, String>,
) -> Result<Listing<T>, Error> {
    let s = top.get().trim();
    let elements: Vec<&RawValue> = match s.as_bytes().first() {
        Some(b'[') => serde_json::from_str(s).map_err(|e| not_json(&e))?,
        Some(b'{') => {
            let OrderedEntries(entries) = serde_json::from_str(s).map_err(|e| not_json(&e))?;
            if let Some(err) = auth_object_error(&entries) {
                return Err(err);
            }
            entries.into_iter().map(|(_, v)| v).collect()
        }
        Some(b'n') | Some(b'f') => return Ok(Listing::default()),
        _ if s == "\"\"" => return Ok(Listing::default()),
        _ => return Err(Error::parse("expected a list")),
    };
    let mut listing = Listing {
        items: Vec::with_capacity(elements.len()),
        skipped: 0,
        errors: Vec::new(),
    };
    for (index, raw) in elements.into_iter().enumerate() {
        if !raw.get().trim_start().starts_with('{') {
            listing.skip(index, "item is not an object");
            continue;
        }
        match convert(raw) {
            Ok(item) => listing.items.push(item),
            Err(message) => listing.skip(index, message),
        }
    }
    Ok(listing)
}

/// Parses a single-object endpoint (`get_vod_info`, `get_series_info`). `[]`, `null`
/// and an empty body yield `T::default()` (panels answer `[]` for unknown ids).
///
/// # Errors
///
/// [`Error::Parse`] when the body is not JSON or not an object, [`Error::Auth`] for an
/// auth-failure object.
pub fn parse_object<T: DeserializeOwned + Default>(body: &[u8]) -> Result<T, Error> {
    let text = body_text(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(T::default());
    }
    let top: &RawValue = serde_json::from_str(trimmed).map_err(|e| not_json(&e))?;
    let s = top.get().trim();
    match s.as_bytes().first() {
        Some(b'{') => {
            let OrderedEntries(entries) = serde_json::from_str(s).map_err(|e| not_json(&e))?;
            if let Some(err) = auth_object_error(&entries) {
                return Err(err);
            }
            serde_json::from_str::<T>(s)
                .map_err(|e| Error::parse(format!("unexpected response shape ({e})")))
        }
        Some(b'[') | Some(b'n') | Some(b'f') => Ok(T::default()),
        _ if s == "\"\"" => Ok(T::default()),
        _ => Err(Error::parse("expected an object")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, Default)]
    struct Probe {
        #[serde(default, deserialize_with = "opt_string")]
        s: Option<String>,
        #[serde(default, deserialize_with = "string")]
        name: String,
        #[serde(default, deserialize_with = "opt_u64")]
        n: Option<u64>,
        #[serde(default, deserialize_with = "opt_u16")]
        port: Option<u16>,
        #[serde(default, deserialize_with = "opt_i64")]
        i: Option<i64>,
        #[serde(default, deserialize_with = "opt_f64")]
        f: Option<f64>,
        #[serde(default, deserialize_with = "bool")]
        b: bool,
        #[serde(default, deserialize_with = "string_list")]
        list: Vec<String>,
        #[serde(default, deserialize_with = "opt_timestamp")]
        ts: Option<DateTime<Utc>>,
    }

    fn probe(json: &str) -> Probe {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn strings_accept_any_scalar() {
        assert_eq!(probe(r#"{"s":"abc"}"#).s.as_deref(), Some("abc"));
        assert_eq!(probe(r#"{"s":"  abc  "}"#).s.as_deref(), Some("abc"));
        assert_eq!(probe(r#"{"s":""}"#).s, None);
        assert_eq!(probe(r#"{"s":"   "}"#).s, None);
        assert_eq!(probe(r#"{"s":null}"#).s, None);
        assert_eq!(probe(r#"{"s":12}"#).s.as_deref(), Some("12"));
        assert_eq!(probe(r#"{"s":12.0}"#).s.as_deref(), Some("12"));
        assert_eq!(probe(r#"{"s":12.5}"#).s.as_deref(), Some("12.5"));
        assert_eq!(probe(r#"{"s":-3}"#).s.as_deref(), Some("-3"));
        assert_eq!(probe(r#"{"s":true}"#).s.as_deref(), Some("true"));
        assert_eq!(probe(r#"{"s":[1,2]}"#).s, None);
        assert_eq!(probe(r#"{"s":{"a":1}}"#).s, None);
        assert_eq!(probe(r#"{}"#).s, None);
        assert_eq!(probe(r#"{"name":null}"#).name, "");
        assert_eq!(probe(r#"{"name":7}"#).name, "7");
    }

    #[test]
    fn integers_accept_numbers_and_numeric_strings() {
        assert_eq!(probe(r#"{"n":5}"#).n, Some(5));
        assert_eq!(probe(r#"{"n":"5"}"#).n, Some(5));
        assert_eq!(probe(r#"{"n":" 5 "}"#).n, Some(5));
        assert_eq!(probe(r#"{"n":"+5"}"#).n, Some(5));
        assert_eq!(probe(r#"{"n":5.0}"#).n, Some(5));
        assert_eq!(probe(r#"{"n":"5.0"}"#).n, Some(5));
        assert_eq!(probe(r#"{"n":5.5}"#).n, None);
        assert_eq!(probe(r#"{"n":-5}"#).n, None);
        assert_eq!(probe(r#"{"n":""}"#).n, None);
        assert_eq!(probe(r#"{"n":"abc"}"#).n, None);
        assert_eq!(probe(r#"{"n":null}"#).n, None);
        assert_eq!(probe(r#"{"n":true}"#).n, None);
        assert_eq!(probe(r#"{"n":[]}"#).n, None);
        assert_eq!(probe(r#"{"n":18446744073709551615}"#).n, Some(u64::MAX));
        assert_eq!(probe(r#"{"i":-5}"#).i, Some(-5));
        assert_eq!(probe(r#"{"i":"-5"}"#).i, Some(-5));
        assert_eq!(probe(r#"{"i":"1700000000"}"#).i, Some(1_700_000_000));
    }

    #[test]
    fn ports_reject_zero_and_overflow() {
        assert_eq!(probe(r#"{"port":"8080"}"#).port, Some(8080));
        assert_eq!(probe(r#"{"port":80}"#).port, Some(80));
        assert_eq!(probe(r#"{"port":"0"}"#).port, None);
        assert_eq!(probe(r#"{"port":"70000"}"#).port, None);
        assert_eq!(probe(r#"{"port":""}"#).port, None);
    }

    #[test]
    fn floats_accept_rating_variants() {
        assert_eq!(probe(r#"{"f":""}"#).f, None);
        assert_eq!(probe(r#"{"f":"7.5"}"#).f, Some(7.5));
        assert_eq!(probe(r#"{"f":7.5}"#).f, Some(7.5));
        assert_eq!(probe(r#"{"f":7}"#).f, Some(7.0));
        assert_eq!(probe(r#"{"f":"7,5"}"#).f, Some(7.5));
        assert_eq!(probe(r#"{"f":"N/A"}"#).f, None);
        assert_eq!(probe(r#"{"f":"NaN"}"#).f, None);
        assert_eq!(probe(r#"{"f":"inf"}"#).f, None);
        assert_eq!(probe(r#"{"f":null}"#).f, None);
    }

    #[test]
    fn bools_accept_php_spellings() {
        for t in [
            r#"true"#,
            r#"1"#,
            r#""1""#,
            r#""true""#,
            r#""TRUE""#,
            r#""yes""#,
            r#"2"#,
        ] {
            assert!(probe(&format!(r#"{{"b":{t}}}"#)).b, "{t} should be true");
        }
        for f in [
            r#"false"#,
            r#"0"#,
            r#""0""#,
            r#""""#,
            r#"null"#,
            r#""false""#,
            r#""no""#,
            r#"[]"#,
            r#""abc""#,
        ] {
            assert!(!probe(&format!(r#"{{"b":{f}}}"#)).b, "{f} should be false");
        }
        assert!(!probe("{}").b);
    }

    #[test]
    fn string_lists_accept_string_or_array() {
        assert_eq!(probe(r#"{"list":"a.jpg"}"#).list, vec!["a.jpg"]);
        assert_eq!(
            probe(r#"{"list":["a.jpg","","b.jpg"]}"#).list,
            vec!["a.jpg", "b.jpg"]
        );
        assert_eq!(probe(r#"{"list":[1,2]}"#).list, vec!["1", "2"]);
        assert_eq!(probe(r#"{"list":""}"#).list, Vec::<String>::new());
        assert_eq!(probe(r#"{"list":null}"#).list, Vec::<String>::new());
        assert_eq!(probe(r#"{"list":{"0":"a","1":"b"}}"#).list, vec!["a", "b"]);
        assert_eq!(
            probe(r#"{"list":[["nested"],{"x":1},"ok"]}"#).list,
            vec!["ok"]
        );
        assert_eq!(probe(r#"{"list":true}"#).list, Vec::<String>::new());
    }

    #[test]
    fn timestamps_from_number_or_string() {
        let expected = DateTime::from_timestamp(1_700_000_000, 0);
        assert_eq!(probe(r#"{"ts":1700000000}"#).ts, expected);
        assert_eq!(probe(r#"{"ts":"1700000000"}"#).ts, expected);
        assert_eq!(probe(r#"{"ts":"0"}"#).ts, None);
        assert_eq!(probe(r#"{"ts":""}"#).ts, None);
        assert_eq!(probe(r#"{"ts":null}"#).ts, None);
        assert_eq!(probe(r#"{"ts":-5}"#).ts, None);
        assert_eq!(probe(r#"{"ts":"null"}"#).ts, None);
        assert_eq!(probe(r#"{"ts":99999999999999999}"#).ts, None);
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Item {
        #[serde(deserialize_with = "required_u64")]
        id: u64,
        #[serde(default, deserialize_with = "string")]
        name: String,
    }

    #[test]
    fn list_skips_malformed_items() {
        let body = br#"[{"id":"1","name":"a"},{"id":"x"},"junk",{"name":"no id"},{"id":2}]"#;
        let listing: Listing<Item> = parse_list(body).unwrap();
        assert_eq!(listing.items.len(), 2);
        assert_eq!(listing.items[0].id, 1);
        assert_eq!(listing.items[1].id, 2);
        assert_eq!(listing.skipped, 3);
        let indices: Vec<_> = listing.errors.iter().map(|e| e.index).collect();
        assert_eq!(indices, vec![1, 2, 3]);
    }

    #[test]
    fn out_of_range_number_only_skips_its_item() {
        // The raw scan accepts 1e400; an ignored field holding it is harmless, a used
        // field makes serde_json reject only that item.
        let listing: Listing<Item> =
            parse_list(br#"[{"id":1,"x":1e400},{"id":2,"name":1e400},{"id":3}]"#).unwrap();
        assert_eq!(
            listing.items.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(listing.skipped, 1);
    }

    #[test]
    fn list_accepts_object_keyed_by_index_and_empty_forms() {
        let listing: Listing<Item> = parse_list(br#"{"0":{"id":1},"1":{"id":2}}"#).unwrap();
        assert_eq!(
            listing.items.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        for empty in [&b""[..], b"  ", b"null", b"false", b"[]", b"{}", b"\"\""] {
            let listing: Listing<Item> = parse_list(empty).unwrap();
            assert!(listing.is_empty());
            assert_eq!(listing.skipped, 0);
        }
    }

    #[test]
    fn list_handles_bom_and_invalid_utf8() {
        let mut body = b"\xEF\xBB\xBF[{\"id\":1,\"name\":\"caf\xE9\"}]".to_vec();
        let listing: Listing<Item> = parse_list(&body).unwrap();
        assert_eq!(listing.items[0].name, "caf\u{FFFD}");
        body.truncate(3);
        assert!(parse_list::<Item>(&body).unwrap().is_empty());
    }

    #[test]
    fn list_maps_auth_failure_object() {
        let err = parse_list::<Item>(br#"{"user_info":{"auth":0}}"#).unwrap_err();
        assert_eq!(
            err,
            Error::Auth {
                status: AuthFailure::InvalidCredentials
            }
        );
        let err = parse_list::<Item>(br#"{"user_info":{"auth":1,"status":"Active"}}"#).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
    }

    #[test]
    fn list_rejects_non_json() {
        let err = parse_list::<Item>(b"<html><body>502 Bad Gateway</body></html>").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
        assert!(!err.to_string().contains("Bad Gateway"));
        assert!(matches!(
            parse_list::<Item>(b"42"),
            Err(Error::Parse { .. })
        ));
        assert!(matches!(
            parse_list::<Item>(b"[{\"id\":1}"),
            Err(Error::Parse { .. })
        ));
    }

    #[test]
    fn skip_report_is_capped() {
        let body = format!("[{}]", vec!["1"; 100].join(","));
        let listing: Listing<Item> = parse_list(body.as_bytes()).unwrap();
        assert_eq!(listing.skipped, 100);
        assert_eq!(listing.errors.len(), MAX_ITEM_ERRORS);
    }

    #[derive(Debug, Deserialize, PartialEq, Default)]
    struct Inner {
        #[serde(default, deserialize_with = "opt_u64")]
        x: Option<u64>,
    }

    #[derive(Debug, Deserialize, PartialEq, Default)]
    struct Outer {
        #[serde(default, deserialize_with = "object_or_default")]
        info: Inner,
        #[serde(default, deserialize_with = "lenient_vec")]
        items: Vec<Item>,
    }

    #[test]
    fn object_or_default_accepts_php_empty_array() {
        let o: Outer = serde_json::from_str(r#"{"info":[]}"#).unwrap();
        assert_eq!(o.info, Inner::default());
        let o: Outer = serde_json::from_str(r#"{"info":{"x":"3"}}"#).unwrap();
        assert_eq!(o.info.x, Some(3));
        let o: Outer = serde_json::from_str(r#"{"info":null}"#).unwrap();
        assert_eq!(o.info, Inner::default());
        let o: Outer = serde_json::from_str(r#"{"info":"oops"}"#).unwrap();
        assert_eq!(o.info, Inner::default());
    }

    #[test]
    fn lenient_vec_drops_bad_elements() {
        let o: Outer =
            serde_json::from_str(r#"{"items":[{"id":1},{"id":"bad"},5,{"id":"2"}]}"#).unwrap();
        assert_eq!(o.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2]);
        let o: Outer = serde_json::from_str(r#"{"items":{"a":{"id":7}}}"#).unwrap();
        assert_eq!(o.items[0].id, 7);
        let o: Outer = serde_json::from_str(r#"{"items":"nope"}"#).unwrap();
        assert!(o.items.is_empty());
    }

    #[test]
    fn parse_object_variants() {
        let o: Outer = parse_object(b"[]").unwrap();
        assert_eq!(o, Outer::default());
        let o: Outer = parse_object(b"").unwrap();
        assert_eq!(o, Outer::default());
        assert!(matches!(
            parse_object::<Outer>(br#"{"user_info":{"auth":"0"}}"#),
            Err(Error::Auth { .. })
        ));
        assert!(matches!(
            parse_object::<Outer>(b"oops"),
            Err(Error::Parse { .. })
        ));
    }
}
