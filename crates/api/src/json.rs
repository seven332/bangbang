use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

#[derive(Debug)]
pub(crate) struct JsonValueWithoutDuplicateObjectKeys(serde_json::Value);

impl JsonValueWithoutDuplicateObjectKeys {
    pub(crate) const fn as_value(&self) -> &serde_json::Value {
        &self.0
    }

    pub(crate) fn into_value(self) -> serde_json::Value {
        self.0
    }
}

pub(crate) fn parse_value_from_str(contents: &str) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_str::<JsonValueWithoutDuplicateObjectKeys>(contents)
        .map(JsonValueWithoutDuplicateObjectKeys::into_value)
}

impl<'de> Deserialize<'de> for JsonValueWithoutDuplicateObjectKeys {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer
            .deserialize_any(JsonValueWithoutDuplicateObjectKeysVisitor)
            .map(Self)
    }
}

#[derive(Debug)]
struct JsonValueWithoutDuplicateObjectKeysVisitor;

impl<'de> Visitor<'de> for JsonValueWithoutDuplicateObjectKeysVisitor {
    type Value = serde_json::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(serde_json::Value::String(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        Deserialize::deserialize(deserializer)
            .map(|value: JsonValueWithoutDuplicateObjectKeys| value.into_value())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));

        while let Some(value) = sequence.next_element::<JsonValueWithoutDuplicateObjectKeys>()? {
            values.push(value.into_value());
        }

        Ok(serde_json::Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = serde_json::Map::new();

        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom("duplicate object key"));
            }

            let value = map.next_value::<JsonValueWithoutDuplicateObjectKeys>()?;
            object.insert(key, value.into_value());
        }

        Ok(serde_json::Value::Object(object))
    }
}
