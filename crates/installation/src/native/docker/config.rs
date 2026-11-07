//! Keep policy decoding unambiguous across serde and Docker's Go decoder.
use crate::error::{Error, Result};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

struct Unique(Value);

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct JsonVisitor;

        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Unique;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object keys")
            }

            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }

            fn visit_bool<E: serde::de::Error>(
                self,
                value: bool,
            ) -> std::result::Result<Unique, E> {
                Ok(Unique(value.into()))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(value.into()))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(value.into()))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| Unique(Value::Number(number)))
                    .ok_or_else(|| E::custom("Invalid JSON number"))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(value.into()))
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(Unique(Value::Array(values)))
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("Duplicate Docker JSON key"));
                    }
                    let Unique(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(Unique(Value::Object(values)))
            }
        }

        deserializer.deserialize_any(JsonVisitor)
    }
}

pub(super) fn decode(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice::<Unique>(bytes)
        .map(|value| value.0)
        .map_err(|_| {
            Error::forbidden("Docker configuration must be valid JSON without duplicate keys.")
        })
}

/// Apply only at Docker struct boundaries, never inside free-form label/option
/// maps. Go also folds Unicode long-s and Kelvin sign into ASCII field names;
/// structural keys must therefore be ASCII, with exact spelling for controls.
pub(super) fn fields(value: &Value, controls: &[&str]) -> Result<()> {
    if let Some(object) = value.as_object() {
        for key in object.keys() {
            if !key.is_ascii()
                || controls
                    .iter()
                    .any(|field| key.eq_ignore_ascii_case(field) && key != field)
            {
                return Err(Error::forbidden(
                    "Docker control fields require their canonical JSON spelling.",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_json_preserves_values_and_rejects_nested_duplicates() {
        let ordinary = br#"{"array":[null,true,false,-7,9,1.25,"text"],"Labels":{"x":"y"}}"#;
        assert_eq!(
            decode(ordinary).unwrap(),
            serde_json::from_slice::<Value>(ordinary).unwrap()
        );
        for duplicate in [
            r#"{"DriverOpts":{"device":"/"},"DriverOpts":{}}"#,
            r#"{"HostConfig":{"RestartPolicy":{"Name":"always","Name":"no"}}}"#,
            r#"{"Mounts":[{"Source":"/","\u0053ource":"/tmp"}]}"#,
        ] {
            assert!(decode(duplicate.as_bytes()).is_err(), "{duplicate}");
        }
    }
}
