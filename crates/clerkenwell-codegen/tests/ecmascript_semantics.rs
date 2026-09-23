//! The artefacts' orderings and number spellings are ECMAScript's. The corpora
//! were produced by `localeCompare` and `String(number)` in Node.

use clerkenwell_codegen::collate::locale_compare;
use clerkenwell_codegen::json::{number_to_string, Json};

fn corpus(name: &str) -> Json {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    Json::parse(&std::fs::read_to_string(path).expect("the corpus is committed"))
        .expect("the corpus is JSON")
}

#[test]
fn keys_sort_as_locale_compare_sorts_them() {
    let expected: Vec<String> = corpus("locale-order.json")
        .as_array()
        .expect("a list")
        .iter()
        .map(|word| word.as_str().expect("a string").to_owned())
        .collect();
    let mut sorted = expected.clone();
    sorted.reverse();
    sorted.sort_by(|left, right| locale_compare(left, right));
    assert_eq!(sorted, expected);
}

#[test]
fn numbers_are_spelled_as_ecmascript_spells_them() {
    for pair in corpus("number-spellings.json").as_array().expect("a list") {
        let [value, spelling] = pair.as_array().expect("a pair") else {
            panic!("each entry is a value and its spelling");
        };
        let value: f64 = value
            .as_str()
            .expect("the value as exact decimal text")
            .parse()
            .expect("a number");
        assert_eq!(
            number_to_string(value),
            spelling.as_str().expect("the spelling"),
            "{value:e}"
        );
    }
}
