//! Validate the contract's JSON limits without creating a plaintext Value tree.
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::{collections::BTreeSet, fmt};
pub(crate) fn validate(bytes: &[u8]) -> Result<(), serde_json::Error> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    Node(0).deserialize(&mut decoder)?;
    decoder.end()
}
struct Node(usize);
impl<'de> DeserializeSeed<'de> for Node {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Node {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        check_text(v)
    }
    fn visit_string<E: de::Error>(self, mut v: String) -> Result<(), E> {
        let result = check_text(&v);
        zeroize::Zeroize::zeroize(&mut v);
        result
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        if self.0 >= 16 {
            return Err(de::Error::custom("depth limit"));
        }
        let mut count = 0;
        while a.next_element_seed(Node(self.0 + 1))?.is_some() {
            count += 1;
            if count > 256 {
                return Err(de::Error::custom("array limit"));
            }
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        if self.0 >= 16 {
            return Err(de::Error::custom("depth limit"));
        }
        let mut keys = BTreeSet::new();
        while let Some(key) = a.next_key::<String>()? {
            check_text(&key)?;
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate key"));
            }
            a.next_value_seed(Node(self.0 + 1))?;
        }
        Ok(())
    }
}
fn check_text<E: de::Error>(v: &str) -> Result<(), E> {
    if v.len() > 16384 {
        Err(E::custom("string limit"))
    } else {
        Ok(())
    }
}
