use serde::{Deserialize, Deserializer, Serializer};

pub(crate) fn one<S: Serializer>(s: S) -> Result<S::Ok, S::Error> {
    s.serialize_i8(1)
}

pub(crate) fn minus_one<S: Serializer>(s: S) -> Result<S::Ok, S::Error> {
    s.serialize_i8(-1)
}
pub(crate) fn exactly<'de, D: Deserializer<'de>>(
    d: D,
    want: i8,
) -> Result<(), D::Error> {
    use serde::de::Error; // needed for custom

    if i8::deserialize(d)? == want {
        Ok(())
    } else {
        Err(D::Error::custom("not this vote"))
    }
}
pub(crate) fn is_one<'de, D: Deserializer<'de>>(d: D) -> Result<(), D::Error> {
    exactly(d, 1)
}
pub(crate) fn is_minus_one<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<(), D::Error> {
    exactly(d, -1)
}
