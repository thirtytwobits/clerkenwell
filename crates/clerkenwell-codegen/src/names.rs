//! Identifier spelling: how definition names become Rust and TypeScript names.

/// Words of `value`: a lower-case letter or digit followed by an upper-case
/// letter starts a new word, and every run of characters outside `[A-Za-z0-9]`
/// separates words.
fn words(value: &str) -> Vec<String> {
    let characters: Vec<char> = value.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, character) in characters.iter().enumerate() {
        if !character.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(*character);
        let camel_boundary = (character.is_ascii_lowercase() || character.is_ascii_digit())
            && characters
                .get(index + 1)
                .is_some_and(char::is_ascii_uppercase);
        if camel_boundary {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// A generated type name: `story.byKey` becomes `StoryByKey`.
pub fn pascal_identifier(value: &str) -> String {
    let joined: String = words(value)
        .iter()
        .map(|word| {
            let mut characters = word.chars();
            let first = characters.next().expect("words are never empty");
            format!("{}{}", first.to_ascii_uppercase(), characters.as_str())
        })
        .collect();
    let candidate = if joined.is_empty() {
        "Value".to_owned()
    } else {
        joined
    };
    if candidate.starts_with(|c: char| c.is_ascii_digit()) {
        format!("Value{candidate}")
    } else {
        candidate
    }
}

/// A fixture string stem: `scenarios.*.worldKey` becomes `scenarios-world-key`.
pub fn kebab_identifier(value: &str) -> String {
    let joined = words(value).join("-").to_ascii_lowercase();
    if joined.is_empty() {
        "value".to_owned()
    } else {
        joined
    }
}

/// A client document key: `world_key` becomes `worldKey`.
pub fn snake_to_camel_identifier(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < characters.len() {
        let next = characters.get(index + 1);
        if characters[index] == '_' && next.is_some_and(char::is_ascii_lowercase) {
            out.push(next.expect("checked above").to_ascii_uppercase());
            index += 2;
        } else {
            out.push(characters[index]);
            index += 1;
        }
    }
    out
}

/// A constant prefix: `authoring.runtime` becomes `AUTHORING_RUNTIME`.
pub fn constant_name(value: &str) -> String {
    let words = words(value);
    if words.is_empty() {
        return "VALUE".to_owned();
    }
    words
        .iter()
        .map(|word| word.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// The constant holding one entity, projection or mutation name.
pub fn name_constant_identifier(value: &str, suffix: &str) -> String {
    format!("{}_{suffix}", constant_name(value))
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while",
];

/// A Rust field name for a wire property, raw where it is a keyword.
pub fn rust_field_identifier(value: &str) -> String {
    let joined = words(value)
        .iter()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("_");
    let candidate = if joined.is_empty() {
        "value".to_owned()
    } else {
        joined
    };
    let prefixed = if candidate.starts_with(|c: char| c.is_ascii_digit()) {
        format!("value_{candidate}")
    } else {
        candidate
    };
    if RUST_KEYWORDS.contains(&prefixed.as_str()) {
        format!("r#{prefixed}")
    } else {
        prefixed
    }
}

/// The helper enum for a string-enum property, scoped by its definition.
pub fn rust_property_enum_name(definition_name: &str, property_name: &str) -> String {
    format!(
        "{}{}",
        pascal_identifier(definition_name),
        pascal_identifier(property_name)
    )
}

/// A TypeScript object key: bare where that is valid syntax, quoted otherwise.
pub fn object_key(key: &str) -> String {
    let mut characters = key.chars();
    let bare = characters
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if bare {
        key.to_owned()
    } else {
        crate::json::string_literal(key)
    }
}

/// Length in UTF-16 code units, the unit ECMAScript measures strings in.
pub fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// ECMAScript `WhiteSpace` and `LineTerminator`, the set `String.prototype.trim` removes.
fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{09}' | '\u{0B}' | '\u{0C}' | '\u{FEFF}' | '\u{0A}' | '\u{0D}' | '\u{2028}' | '\u{2029}'
    ) || (character != '\u{85}' && character.is_whitespace())
}

pub fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

pub fn js_trim_start(value: &str) -> &str {
    value.trim_start_matches(is_js_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_boundaries_and_punctuation_both_separate_words() {
        assert_eq!(pascal_identifier("story.byKey"), "StoryByKey");
        assert_eq!(constant_name("authoring.runtime"), "AUTHORING_RUNTIME");
        assert_eq!(
            kebab_identifier("scenarios.*.worldKey"),
            "scenarios-world-key"
        );
        assert_eq!(rust_field_identifier("selfReference"), "self_reference");
    }

    #[test]
    fn identifiers_are_never_empty() {
        for value in ["", "...", "9lives", "é"] {
            for identifier in [
                pascal_identifier(value),
                constant_name(value),
                kebab_identifier(value),
                rust_field_identifier(value),
            ] {
                assert!(!identifier.is_empty(), "{value:?}");
            }
        }
    }

    #[test]
    fn type_and_field_identifiers_never_start_with_a_digit() {
        for value in ["9lives", "1.2", "0"] {
            for identifier in [pascal_identifier(value), rust_field_identifier(value)] {
                assert!(
                    !identifier.starts_with(|c: char| c.is_ascii_digit()),
                    "{value:?}"
                );
            }
        }
    }

    #[test]
    fn rust_keywords_become_raw_identifiers() {
        for keyword in RUST_KEYWORDS
            .iter()
            .filter(|k| k.chars().all(|c| c.is_ascii_lowercase()))
        {
            assert_eq!(rust_field_identifier(keyword), format!("r#{keyword}"));
        }
    }

    #[test]
    fn snake_case_keys_become_camel_case_and_nothing_else_changes() {
        assert_eq!(snake_to_camel_identifier("world_key"), "worldKey");
        assert_eq!(snake_to_camel_identifier("already"), "already");
        assert_eq!(snake_to_camel_identifier("trailing_"), "trailing_");
        assert_eq!(snake_to_camel_identifier("a_1"), "a_1");
    }
}
