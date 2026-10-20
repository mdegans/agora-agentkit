use schemars::json_schema;

pub(crate) fn one(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({ "const": 1 })
}

pub(crate) fn minus_one(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({ "const": 1 })
}
