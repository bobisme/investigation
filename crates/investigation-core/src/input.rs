//! Bounded JSON for versioned requests and journals. Original evidence is retained verbatim.

use std::{cmp::Ordering, collections::BTreeMap, fmt};

use schemars::JsonSchema;
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Number, Value};

pub const MAX_REQUEST_BYTES: usize = 512 * 1024;
pub const MAX_REQUEST_DEPTH: usize = 32;
const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputErrorKind {
    TooLarge,
    TooDeep,
    InvalidUtf8,
    DuplicateKey,
    UnsafeNumber,
    InvalidJson,
    InvalidField,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError {
    pub kind: InputErrorKind,
    pub path: String,
    pub message: &'static str,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for InputError {}

/// Parse duplicate-free JSON without interpreting any field as domain data.
/// Paths in reported errors use JSON Pointer, with an empty string for the root.
pub fn parse_request(bytes: &[u8]) -> Result<Value, InputError> {
    parse_json(bytes, MAX_REQUEST_BYTES, MAX_REQUEST_DEPTH)
}

/// Parse a JSON document with caller-defined bounds. Evidence parsers and journal
/// readers can select limits separately from the versioned command envelope.
pub fn parse_json(bytes: &[u8], max_bytes: usize, max_depth: usize) -> Result<Value, InputError> {
    if bytes.len() > max_bytes {
        return Err(root_error(
            InputErrorKind::TooLarge,
            "JSON exceeds its byte limit",
        ));
    }
    let input = std::str::from_utf8(bytes)
        .map_err(|_| root_error(InputErrorKind::InvalidUtf8, "JSON must be UTF-8"))?;
    let mut decoder = serde_json::Deserializer::from_str(input);
    let mut context = DecodeContext {
        segments: Vec::new(),
        max_depth,
        error: None,
    };
    let result = Seed(&mut context)
        .deserialize(&mut decoder)
        .and_then(|value| decoder.end().map(|()| value));
    result.map_err(|_| {
        context.error.unwrap_or_else(|| {
            root_error(
                InputErrorKind::InvalidJson,
                "Malformed JSON or trailing content",
            )
        })
    })
}

/// Decode an already checked value into a serde type. The raw serde error may
/// contain input text, so expose its path and a fixed message only.
pub fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, InputError> {
    serde_path_to_error::deserialize(value).map_err(|err| InputError {
        kind: InputErrorKind::InvalidField,
        path: pointer(err.path().iter().filter_map(|segment| match segment {
            serde_path_to_error::Segment::Seq { index } => Some(index.to_string()),
            serde_path_to_error::Segment::Map { key } => Some(key.clone()),
            serde_path_to_error::Segment::Enum { variant } => Some(variant.clone()),
            serde_path_to_error::Segment::Unknown => None,
        })),
        message: "Field does not match the expected schema",
    })
}

fn root_error(kind: InputErrorKind, message: &'static str) -> InputError {
    InputError {
        kind,
        path: String::new(),
        message,
    }
}

pub(crate) fn pointer(segments: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let mut path = String::new();
    for segment in segments {
        path.push('/');
        let segment = segment.as_ref();
        // Keep machine errors bounded even for an adversarial field name.
        for character in segment.chars().take(128) {
            match character {
                '~' => path.push_str("~0"),
                '/' => path.push_str("~1"),
                character => path.push(character),
            }
        }
    }
    path
}

struct DecodeContext {
    segments: Vec<String>,
    max_depth: usize,
    error: Option<InputError>,
}

impl DecodeContext {
    fn reject<E: de::Error>(&mut self, kind: InputErrorKind, message: &'static str) -> E {
        self.error = Some(InputError {
            kind,
            path: pointer(&self.segments),
            message,
        });
        E::custom(message)
    }
}

struct Seed<'a>(&'a mut DecodeContext);

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;

    fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        if self.0.segments.len() > self.0.max_depth {
            return Err(self
                .0
                .reject(InputErrorKind::TooDeep, "JSON exceeds its nesting limit"));
        }
        decoder.deserialize_any(Checked(self.0)).inspect_err(|_| {
            if self.0.error.is_none() {
                self.0.error = Some(InputError {
                    kind: InputErrorKind::InvalidJson,
                    path: pointer(&self.0.segments),
                    message: "Malformed JSON",
                });
            }
        })
    }
}

struct Checked<'a>(&'a mut DecodeContext);

impl<'de> Visitor<'de> for Checked<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
            return Ok(Value::Number(value.into()));
        }
        Err(self.0.reject(
            InputErrorKind::UnsafeNumber,
            "Encode large integers as strings",
        ))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        if value <= MAX_SAFE_INTEGER as u64 {
            return Ok(Value::Number(value.into()));
        }
        Err(self.0.reject(
            InputErrorKind::UnsafeNumber,
            "Encode large integers as strings",
        ))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        if value.abs() > MAX_SAFE_INTEGER as f64 && value.fract() == 0.0 {
            return Err(self.0.reject(
                InputErrorKind::UnsafeNumber,
                "Encode large integers as strings",
            ));
        }
        Number::from_f64(value).map(Value::Number).ok_or_else(|| {
            self.0
                .reject(InputErrorKind::UnsafeNumber, "JSON numbers must be finite")
        })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        loop {
            self.0.segments.push(values.len().to_string());
            let next = access.next_element_seed(Seed(self.0));
            self.0.segments.pop();
            match next? {
                Some(value) => values.push(value),
                None => return Ok(Value::Array(values)),
            }
        }
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut values = BTreeMap::new();
        while let Some(key) = access.next_key::<String>()? {
            self.0.segments.push(key.clone());
            if values.contains_key(&key) {
                return Err(self.0.reject(
                    InputErrorKind::DuplicateKey,
                    "JSON object contains a duplicate key",
                ));
            }
            let value = access.next_value_seed(Seed(self.0));
            self.0.segments.pop();
            values.insert(key, value?);
        }
        Ok(Value::Object(values.into_iter().collect()))
    }
}

/// A measured decimal with exact spelling, including trailing zeroes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DecimalString(String);

impl JsonSchema for DecimalString {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DecimalString".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "pattern": r"^-?(0|[1-9][0-9]*)(\.[0-9]+)?$" })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    pub decimal: DecimalString,
    #[schemars(length(min = 1))]
    pub unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<u64>,
}

impl DecimalString {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compare numerical values without discarding the stored precision.
    pub fn numeric_cmp(&self, other: &Self) -> Ordering {
        fn parts(value: &str) -> (bool, &str, &str) {
            let unsigned = value.strip_prefix('-').unwrap_or(value);
            let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
            let fraction = fraction.trim_end_matches('0');
            let negative = value.starts_with('-') && (whole != "0" || !fraction.is_empty());
            (negative, whole, fraction)
        }

        let (left_negative, left_whole, left_fraction) = parts(self.as_str());
        let (right_negative, right_whole, right_fraction) = parts(other.as_str());
        if left_negative != right_negative {
            return right_negative.cmp(&left_negative);
        }
        let magnitude = left_whole
            .len()
            .cmp(&right_whole.len())
            .then_with(|| left_whole.cmp(right_whole))
            .then_with(|| left_fraction.cmp(right_fraction));
        if left_negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }
}

impl From<DecimalString> for String {
    fn from(value: DecimalString) -> Self {
        value.0
    }
}

impl TryFrom<String> for DecimalString {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let unsigned = value.strip_prefix('-').unwrap_or(&value);
        let (whole, fraction) = unsigned
            .split_once('.')
            .map_or((unsigned, None), |(whole, fraction)| {
                (whole, Some(fraction))
            });
        let digits =
            |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        if !digits(whole)
            || (whole.len() > 1 && whole.starts_with('0'))
            || fraction.is_some_and(|part| !digits(part))
        {
            return Err("Expected a base-10 decimal string such as 18.70");
        }
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_json_at_the_failing_field() {
        for (bytes, path, kind) in [
            (
                br#"{"items":[{"a/b":1,"a/b":2}]}"#.as_slice(),
                "/items/0/a~1b",
                InputErrorKind::DuplicateKey,
            ),
            (
                br#"{"n":9007199254740992}"#.as_slice(),
                "/n",
                InputErrorKind::UnsafeNumber,
            ),
            (
                br#"{"n":-9007199254740992}"#.as_slice(),
                "/n",
                InputErrorKind::UnsafeNumber,
            ),
            (
                br#"{"n":18446744073709551616}"#.as_slice(),
                "/n",
                InputErrorKind::UnsafeNumber,
            ),
            (
                br#"{"n":NaN}"#.as_slice(),
                "/n",
                InputErrorKind::InvalidJson,
            ),
            (
                br#"{"n":1e999}"#.as_slice(),
                "/n",
                InputErrorKind::InvalidJson,
            ),
            ([255_u8].as_slice(), "", InputErrorKind::InvalidUtf8),
            (b"{} []".as_slice(), "", InputErrorKind::InvalidJson),
        ] {
            let error = parse_request(bytes).unwrap_err();
            assert_eq!((error.path.as_str(), error.kind), (path, kind));
        }
        assert!(parse_request(br#"{"n":9007199254740991}"#).is_ok());
    }

    #[test]
    fn decimal_comparison_uses_numbers_and_keeps_original_spelling() {
        for (left, right, order) in [
            ("-0.00", "0", Ordering::Equal),
            ("-999999999999999999999.12", "-9", Ordering::Less),
            ("1.000000000000000000000001", "1", Ordering::Greater),
            ("18.70", "18.7", Ordering::Equal),
            ("18.70", "18.69", Ordering::Greater),
        ] {
            let left = DecimalString::try_from(left.to_owned()).unwrap();
            let right = DecimalString::try_from(right.to_owned()).unwrap();
            assert_eq!(left.numeric_cmp(&right), order);
            assert_eq!(right.numeric_cmp(&left), order.reverse());
        }
    }

    #[test]
    fn denies_unknown_measurement_field_and_reports_path() {
        let value = parse_request(br#"{"decimal":"18.70","unit":"ms","estimate":true}"#).unwrap();
        let error = decode::<Measurement>(&value).unwrap_err();
        assert_eq!(error.path, "/estimate");
    }

    #[test]
    fn enforces_size_and_depth_and_preserves_decimal_spelling() {
        assert_eq!(
            parse_json(b"{}", 1, 4).unwrap_err().kind,
            InputErrorKind::TooLarge
        );
        assert_eq!(
            parse_json(br#"{"a":[1]}"#, 32, 1).unwrap_err().kind,
            InputErrorKind::TooDeep
        );
        let value = parse_request(br#""18.70""#).unwrap();
        let decimal = decode::<DecimalString>(&value).unwrap();
        assert_eq!(decimal.as_str(), "18.70");
        assert_eq!(serde_json::to_value(decimal).unwrap(), value);
        for bad in ["", "+1", "01", "1e3", "1.", "NaN"] {
            assert!(
                decode::<DecimalString>(&Value::String(bad.into())).is_err(),
                "{bad}"
            );
        }
    }
}
