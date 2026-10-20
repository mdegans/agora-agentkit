//! Inline schemas for values serde writes by hand (`VoteValue`'s `1` and
//! `-1`)

use schemars::json_schema;

pub(crate) fn one(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({ "type": "integer", "const": 1 })
}

pub(crate) fn minus_one(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({ "type": "integer", "const": -1 })
}

/// `1` or `-1`: a vote on content, without the ballot (`cast_vote`'s
/// `value`; its ballot has a field of its own)
pub(crate) fn up_or_down(
    generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    json_schema!({ "anyOf": [one(generator), minus_one(generator)] })
}
