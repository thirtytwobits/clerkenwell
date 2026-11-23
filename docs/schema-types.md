# Schema types

A definition's `$defs` entries are JSON Schema in a subset each binding renders exactly. A
schema outside it is refused at validation or generation, never widened to `unknown`.
`clerkenwell-codegen` imports JSON Schema written by other tools into the same subset.

## The subset

| Schema | Rust | TypeScript |
|---|---|---|
| `{"type": "string"}` | `String` | `string` |
| `{"type": "boolean"}` | `bool` | `boolean` |
| `{"type": "integer"}` | `i64` | `number` |
| `{"type": "number"}` | `f64` | `number` |
| `{"type": "null"}` | `()` | `null` |
| `{"type": "json"}`, any JSON value | `JsonValue` | `unknown` |
| `{"type": "string", "enum": [...]}` | an enum | a union of the values |
| `{"type": ["string", "integer"]}`, a scalar union | an untagged enum; a `$defs` entry only | a union |
| `{"type": [T, "null"]}`, `{"anyOf": [S, {"type": "null"}]}` | `Option<T>` | `T \| null` |
| `{"type": "array", "items": S}` | `Vec<S>` | `S[]` |
| an object with `"additionalProperties": false` | a struct refusing other members | an object type |
| an object with `properties` and an `additionalProperties` schema | a struct with `OtherMembers` | an object type with an index signature |
| an object with an `additionalProperties` schema only, a record | `RecordMembers` | `Record<string, S>` |
| an internally tagged union | an enum tagged by its `discriminator` | a union of objects carrying the tag |
| an externally tagged union | an enum in serde's default representation | a union of one-member objects and tags |
| `{"$ref": "#/$defs/Name"}` | `Name` | `Name` |

A `$defs` entry that is a scalar, a nullable schema or an array is an alias of its value's type.
Each use of the alias carries the entry's validation keywords.

### Members

A required member has its value's type. An optional member is `Option<T>` in Rust and `?:` in
TypeScript. An optional member whose values include `null` reads absent as `None`, `null` as
`Some` of its null value and any other value as `Some`, through `clerkenwell_schema::present`, and
writes each back as it was read.

An open object's declared members are values of its `additionalProperties` schema, as a
TypeScript index signature requires.

### Unions

A union is a `$defs` entry. `oneOf` maps each tag to its variant.

- **Internally tagged**: `"type": "object"`, `discriminator` naming the member that carries the
  tag, and each variant an object schema without that member. A variant with an
  `additionalProperties` schema is open: Rust generates it as a struct named after the union and
  the tag, which declares the tag among its members.
- **Externally tagged**: no `type` and no `discriminator`. Each data variant is written as an
  object of one member named by its tag, whose value is the variant's schema; `enum` lists the
  unit variants, each written as its tag. A data variant whose value is an open object is
  generated as a struct named after the union and the tag.

### Validation keywords and annotations

`minLength`, `minimum`, `maximum`, `const` and `default` constrain values; the generated Rust
types carry them into the schema `schemars` derives. `description` and `title` annotate any
schema. `enumDescriptions` maps a string enum's values to descriptions; Rust documents each
variant with its description, and the generated JSON Schema writes the enum as described
constants.

## Importing JSON Schema

The generator's `schemaImports` lists JSON Schema bundles with `$defs`. Each definition name has
one owner among the definition and its bundles. A definition imports the definitions it reaches:
those its sections and `$defs` reference, and those they reference in turn. A definition it does
not reach is not converted.

| JSON Schema | Imported as |
|---|---|
| `true`, a schema of annotations only | any JSON value |
| `$comment`, `deprecated`, `examples`, `readOnly`, `writeOnly`, `contentEncoding`, `contentMediaType`, `contentSchema` | dropped: annotations only |
| no `type` beside `const`, `enum`, `properties`, `additionalProperties` or `items` | the type it implies |
| `format` on a string | dropped: an annotation |
| `format` `int8`, `int16`, `int32`, `uint8`, `uint16`, `uint32` | an integer within that range |
| `format` `int64`, `int` | an integer |
| `format` `uint64`, `uint` | an integer of at least 0, held as an `i64` |
| `format` `float` | a number within the finite `f32` range |
| `format` `double` | a number |
| `exclusiveMinimum`, `exclusiveMaximum` on an integer | the inclusive bound one integer inside it |
| `{"type": [T, "null"]}`, `anyOf` or `oneOf` pairing one schema with `{"type": "null"}` | that schema, nullable |
| `oneOf` of string constants and string enums | a string enum, each constant's description in `enumDescriptions` |
| `oneOf` of objects that each require one member holding a string constant, under one name | an internally tagged union |
| `oneOf` of closed objects of one required member, and string constants | an externally tagged union |
| an object without `additionalProperties`, or with `true` | an open object whose other members are any JSON value; a record when it declares no member |

These are the forms serde and `schemars` write for structs, `Option`, enums in each
representation serde offers but untagged, newtypes and Rust's integer and float types.

### Refused

A definition a definition reaches is refused, naming its path, when it uses any of:

- `pattern`, `maxLength`, `multipleOf`, `minItems`, `maxItems`, `uniqueItems`, `contains`,
  `prefixItems`, `minProperties`, `maxProperties`, `propertyNames`, `patternProperties`,
  `dependentRequired`, `dependentSchemas`, `if`, `then`, `else`, `not`, `allOf` or the
  `unevaluated` keywords;
- an `anyOf` other than one schema paired with `null`;
- a `oneOf` the subset holds no union form of, such as serde's untagged representation;
- an `enum` of values other than strings;
- a 128-bit integer format, or `exclusiveMinimum` or `exclusiveMaximum` on a number.
