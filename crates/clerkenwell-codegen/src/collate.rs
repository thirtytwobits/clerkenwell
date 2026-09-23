//! The two string orders the artefacts are sorted by.

use std::cmp::Ordering;
use std::sync::OnceLock;

use icu_collator::{Collator, CollatorBorrowed};

/// `String.prototype.localeCompare` under the root collation (the `en` one):
/// Unicode collation at tertiary strength, punctuation not ignored.
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR
        .get_or_init(|| {
            Collator::try_new(Default::default(), Default::default())
                .expect("the root collation is compiled into icu_collator")
        })
        .compare(left, right)
}

/// `Array.prototype.sort` with no comparator: UTF-16 code unit order.
pub fn code_unit_compare(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_order_puts_case_second_to_letters() {
        assert_eq!(locale_compare("a", "B"), Ordering::Less);
        assert_eq!(locale_compare("b", "A"), Ordering::Greater);
        assert_eq!(locale_compare("a", "A"), Ordering::Less);
        assert_eq!(code_unit_compare("a", "B"), Ordering::Greater);
    }

    #[test]
    fn locale_order_puts_punctuation_before_digits_before_letters() {
        for punctuation in ["_", "-", ".", "*", "$"] {
            assert_eq!(
                locale_compare(punctuation, "0"),
                Ordering::Less,
                "{punctuation}"
            );
        }
        assert_eq!(locale_compare("9", "a"), Ordering::Less);
    }

    #[test]
    fn code_unit_order_compares_utf16_not_scalar_values() {
        // U+FF5E is one code unit; U+1F600 is a surrogate pair starting 0xD83D.
        assert_eq!(code_unit_compare("\u{1F600}", "\u{FF5E}"), Ordering::Less);
        assert_eq!("\u{1F600}".cmp("\u{FF5E}"), Ordering::Greater);
    }
}
