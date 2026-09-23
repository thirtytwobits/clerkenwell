//! rustfmt's layout (edition 2021, default configuration) of the expressions
//! the Rust model's registry items are made of.
//!
//! An item that rustfmt cannot fit within `max_width` in any layout is left
//! exactly as written, so such an item keeps the plain layout: arrays of
//! structs and structs vertical, everything else on one line.

const MAX_WIDTH: usize = 100;
const TAB: usize = 4;
/// `array_width`: an array wider than this goes vertical.
const ARRAY_WIDTH: usize = 60;
/// `fn_call_width`: call arguments wider than this go vertical.
const FN_CALL_WIDTH: usize = 60;
/// `struct_lit_width`: struct literal fields wider than this go vertical.
const STRUCT_LIT_WIDTH: usize = 18;
/// `short_array_element_width_threshold`: an array of elements no wider than
/// this fills lines instead of going vertical.
const SHORT_ARRAY_ELEMENT_WIDTH: usize = 10;

/// A Rust expression as the registry items spell them.
#[derive(Clone, Debug)]
pub(crate) enum Expr {
    /// A literal or a path, which no layout breaks.
    Atom(String),
    /// `&expr`.
    Ref(Box<Expr>),
    /// `[items]`.
    Array(Vec<Expr>),
    /// `callee(arguments)`.
    Call(String, Vec<Expr>),
    /// `(items)`.
    Tuple(Vec<Expr>),
    /// `Name { field: value, .. }`.
    Struct(String, Vec<(String, Expr)>),
}

impl Expr {
    pub(crate) fn atom(text: impl Into<String>) -> Expr {
        Expr::Atom(text.into())
    }

    /// `&[items]`.
    pub(crate) fn slice(items: Vec<Expr>) -> Expr {
        Expr::Ref(Box::new(Expr::Array(items)))
    }

    /// `None`, or `Some(value)`.
    pub(crate) fn option(value: Option<Expr>) -> Expr {
        match value {
            Some(value) => Expr::Call("Some".to_owned(), vec![value]),
            None => Expr::atom("None"),
        }
    }

    fn is_atom(&self) -> bool {
        matches!(self, Expr::Atom(_))
    }

    /// Whether rustfmt may lay this out against its delimiter when it is a
    /// list's only element.
    fn is_overflowable(&self) -> bool {
        match self {
            Expr::Array(_) | Expr::Tuple(_) | Expr::Struct(..) | Expr::Call(..) => true,
            Expr::Ref(inner) => inner.is_overflowable(),
            Expr::Atom(_) => false,
        }
    }
}

/// Where an expression starts: the block indent its continuation lines hang
/// from, and the width left on its first line.
#[derive(Clone, Copy, Debug)]
struct Shape {
    indent: usize,
    width: usize,
}

impl Shape {
    fn nested(self) -> Shape {
        let indent = self.indent + TAB;
        Shape {
            indent,
            width: MAX_WIDTH.saturating_sub(indent + 1),
        }
    }
}

fn spaces(count: usize) -> String {
    " ".repeat(count)
}

fn width(text: &str) -> usize {
    text.chars().count()
}

fn is_single_line(text: &str) -> bool {
    !text.contains('\n')
}

fn rewrite(expr: &Expr, shape: Shape) -> Option<String> {
    match expr {
        Expr::Atom(text) => (width(text) <= shape.width).then(|| text.clone()),
        Expr::Ref(inner) => rewrite(
            inner,
            Shape {
                indent: shape.indent,
                width: shape.width.checked_sub(1)?,
            },
        )
        .map(|inner| format!("&{inner}")),
        Expr::Array(items) => rewrite_list("[", "]", items, shape, ARRAY_WIDTH),
        Expr::Tuple(items) => rewrite_list("(", ")", items, shape, FN_CALL_WIDTH),
        Expr::Call(callee, arguments) => {
            rewrite_list(&format!("{callee}("), ")", arguments, shape, FN_CALL_WIDTH)
        }
        Expr::Struct(name, fields) => rewrite_struct(name, fields, shape),
    }
}

fn rewrite_list(
    open: &str,
    close: &str,
    items: &[Expr],
    shape: Shape,
    item_max_width: usize,
) -> Option<String> {
    let one_line_width = shape.width.checked_sub(width(open) + width(close))?;
    if items.is_empty() {
        return Some(format!("{open}{close}"));
    }
    let inline = Shape {
        indent: shape.indent,
        width: one_line_width,
    };
    if let [only] = items {
        // A lone element stays on the line whenever it fits there.
        if let Some(single) = rewrite(only, inline).filter(|text| is_single_line(text)) {
            return Some(format!("{open}{single}{close}"));
        }
        if only.is_overflowable() {
            if let Some(overflowed) = rewrite(only, inline) {
                return Some(format!("{open}{overflowed}{close}"));
            }
        }
    } else {
        let singles: Option<Vec<String>> = items
            .iter()
            .map(|item| rewrite(item, inline).filter(|text| is_single_line(text)))
            .collect();
        if let Some(singles) = singles {
            let total =
                singles.iter().map(|text| width(text)).sum::<usize>() + 2 * (singles.len() - 1);
            if total <= one_line_width.min(item_max_width) {
                return Some(format!("{open}{}{close}", singles.join(", ")));
            }
            let short = items.iter().all(Expr::is_atom)
                && singles
                    .iter()
                    .all(|text| width(text) <= SHORT_ARRAY_ELEMENT_WIDTH);
            if short {
                return Some(mixed(open, close, &singles, shape));
            }
        }
    }
    vertical(open, close, items, shape)
}

/// One element per line, each followed by a comma.
fn vertical(open: &str, close: &str, items: &[Expr], shape: Shape) -> Option<String> {
    let nested = shape.nested();
    let mut text = format!("{open}\n");
    for item in items {
        let item = rewrite(item, nested)?;
        text.push_str(&format!("{}{item},\n", spaces(nested.indent)));
    }
    text.push_str(&format!("{}{close}", spaces(shape.indent)));
    Some(text)
}

/// Short elements filling each line. The last element's comma counts toward
/// the fit only once the list has wrapped.
fn mixed(open: &str, close: &str, items: &[String], shape: Shape) -> String {
    let nested = shape.nested();
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut wrapped = false;
    for (index, item) in items.iter().enumerate() {
        let last = index + 1 == items.len();
        let item_width = width(item) + usize::from(!last || wrapped);
        if !line.is_empty() && width(&line) + 1 + item_width > nested.width {
            lines.push(std::mem::take(&mut line));
            wrapped = true;
        } else if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(item);
        line.push(',');
    }
    lines.push(line);
    let body: String = lines
        .iter()
        .map(|line| format!("{}{line}\n", spaces(nested.indent)))
        .collect();
    format!("{open}\n{body}{}{close}", spaces(shape.indent))
}

fn rewrite_struct(name: &str, fields: &[(String, Expr)], shape: Shape) -> Option<String> {
    let opening = format!("{name} {{");
    if width(&opening) > shape.width {
        return None;
    }
    let horizontal: Option<Vec<String>> = fields
        .iter()
        .map(|(field, value)| {
            rewrite(
                value,
                Shape {
                    indent: shape.indent,
                    width: MAX_WIDTH,
                },
            )
            .filter(|text| is_single_line(text))
            .map(|value| format!("{field}: {value}"))
        })
        .collect();
    if let Some(horizontal) = horizontal {
        let body = horizontal.join(", ");
        if width(&body) <= STRUCT_LIT_WIDTH && width(&body) + width(&opening) + 2 <= shape.width {
            return Some(format!("{opening} {body} }}"));
        }
    }
    let field_indent = shape.indent + TAB;
    let mut text = format!("{opening}\n");
    for (field, value) in fields {
        text.push_str(&spaces(field_indent));
        text.push_str(&rewrite_field(field, value, field_indent)?);
        text.push_str(",\n");
    }
    text.push_str(&format!("{}}}", spaces(shape.indent)));
    Some(text)
}

/// `field: value` on one line when the value's first line fits there (the
/// comma reserved), otherwise the value alone on the next line.
fn rewrite_field(field: &str, value: &Expr, indent: usize) -> Option<String> {
    let same_line = Shape {
        indent,
        width: MAX_WIDTH.checked_sub(indent + width(field) + 2 + 1)?,
    };
    if let Some(value) = rewrite(value, same_line) {
        return Some(format!("{field}: {value}"));
    }
    let next_line = Shape {
        indent: indent + TAB,
        width: MAX_WIDTH.checked_sub(indent + TAB)?,
    };
    rewrite(value, next_line).map(|value| format!("{field}:\n{}{value}", spaces(indent + TAB)))
}

fn newlines(text: &str) -> usize {
    text.matches('\n').count()
}

fn first_line_ends_with(text: &str, character: char) -> bool {
    text.lines()
        .next()
        .is_some_and(|line| line.ends_with(character))
}

/// Whether rustfmt moves an item's right-hand side to the next line.
fn prefer_next_line(same_line: &str, next_line: &str) -> bool {
    is_single_line(next_line)
        || newlines(same_line) > newlines(next_line) + 1
        || ['(', '{', '[']
            .iter()
            .any(|&c| first_line_ends_with(same_line, c) && !first_line_ends_with(next_line, c))
}

/// `left value;` as rustfmt lays it out, or `None` where rustfmt cannot.
fn rustfmt_item(left: &str, value: &Expr) -> Option<String> {
    let same_line = MAX_WIDTH
        .checked_sub(width(left) + 2)
        .and_then(|available| {
            rewrite(
                value,
                Shape {
                    indent: 0,
                    width: available,
                },
            )
        });
    if let Some(same) = same_line.as_deref().filter(|text| is_single_line(text)) {
        return Some(format!("{left} {same};"));
    }
    let next_line = rewrite(
        value,
        Shape {
            indent: TAB,
            width: MAX_WIDTH - TAB - 1,
        },
    );
    let chosen = match (same_line, next_line) {
        (Some(same), Some(next)) if prefer_next_line(&same, &next) => {
            format!("\n{}{next}", spaces(TAB))
        }
        (Some(same), _) => format!(" {same}"),
        (None, Some(next)) => format!("\n{}{next}", spaces(TAB)),
        (None, None) => return None,
    };
    Some(format!("{left}{chosen};"))
}

/// The plain layout of an expression starting at `indent`.
fn written(value: &Expr, indent: usize) -> String {
    match value {
        Expr::Atom(text) => text.clone(),
        Expr::Ref(inner) => format!("&{}", written(inner, indent)),
        Expr::Array(items) if items.iter().any(|item| matches!(item, Expr::Struct(..))) => {
            let nested = indent + TAB;
            let body: String = items
                .iter()
                .map(|item| format!("{}{},\n", spaces(nested), written(item, nested)))
                .collect();
            format!("[\n{body}{}]", spaces(indent))
        }
        Expr::Array(items) => format!("[{}]", written_list(items, indent)),
        Expr::Tuple(items) => format!("({})", written_list(items, indent)),
        Expr::Call(callee, arguments) => format!("{callee}({})", written_list(arguments, indent)),
        Expr::Struct(name, fields) => {
            let nested = indent + TAB;
            let body: String = fields
                .iter()
                .map(|(field, value)| {
                    format!("{}{field}: {},\n", spaces(nested), written(value, nested))
                })
                .collect();
            format!("{name} {{\n{body}{}}}", spaces(indent))
        }
    }
}

fn written_list(items: &[Expr], indent: usize) -> String {
    items
        .iter()
        .map(|item| written(item, indent))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `line` fits within rustfmt's `max_width`.
pub(crate) fn fits(line: &str) -> bool {
    width(line) <= MAX_WIDTH
}

/// Whether an attribute line stays on one line: rustfmt keeps a column spare.
pub(crate) fn fits_attribute(line: &str) -> bool {
    width(line) < MAX_WIDTH
}

/// A declaration `head value,` such as a struct field, with the value on the
/// next line when the line is too wide.
pub(crate) fn declaration(indent: &str, head: &str, value: &str) -> String {
    let line = format!("{indent}{head} {value},");
    if fits(&line) {
        line
    } else {
        format!("{indent}{head}\n{indent}{}{value},", spaces(TAB))
    }
}

/// `open argument close`, with the lone argument on a line of its own when
/// the line is too wide. `open` carries the line's indentation.
pub(crate) fn wrap_last_argument(open: &str, argument: &str, close: &str) -> String {
    let line = format!("{open}{argument}{close}");
    if fits(&line) {
        return line;
    }
    let indent = &open[..open.len() - open.trim_start().len()];
    format!(
        "{open}\n{indent}{}{argument},\n{indent}{close}",
        spaces(TAB)
    )
}

/// `left value;` as rustfmt formats it; where rustfmt cannot, the plain
/// layout, with a struct value on the line after `left`.
pub(crate) fn item(left: &str, value: &Expr) -> String {
    rustfmt_item(left, value).unwrap_or_else(|| match value {
        Expr::Struct(..) => format!("{left}\n{}{};", spaces(TAB), written(value, TAB)),
        _ => format!("{left} {};", written(value, 0)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lits(values: &[&str]) -> Vec<Expr> {
        values
            .iter()
            .map(|value| Expr::atom(format!("{value:?}")))
            .collect()
    }

    #[test]
    fn a_short_list_stays_on_the_line() {
        let rendered = item(
            "pub const NAMES: &[&str] =",
            &Expr::slice(lits(&["a", "b"])),
        );
        assert!(!rendered.contains('\n'), "{rendered}");
    }

    /// Where rustfmt gives up on an item it leaves it as written, so the item
    /// keeps the plain layout rather than a partial rustfmt one.
    #[test]
    fn an_item_rustfmt_cannot_fit_keeps_the_plain_layout() {
        let long = "x".repeat(120);
        let list = lits(&["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"]);
        let value = Expr::slice(vec![Expr::Struct(
            "Spec".to_owned(),
            vec![
                ("text".to_owned(), Expr::atom(format!("{long:?}"))),
                ("list".to_owned(), Expr::slice(list.clone())),
            ],
        )]);
        assert!(rustfmt_item("pub static SPECS: &[Spec] =", &value).is_none());
        let plain = item("pub static SPECS: &[Spec] =", &value);
        let one_line_list = written(&Expr::slice(list), 0);
        assert!(plain.contains(&one_line_list), "{plain}");
    }

    /// A small deterministic generator, so the corpus is the same every run.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % bound
        }

        fn text(&mut self, shortest: usize, longest: usize) -> String {
            let length = shortest + self.below(longest - shortest + 1);
            "x".repeat(length)
        }
    }

    fn random_atom(rng: &mut Lcg) -> Expr {
        match rng.below(3) {
            0 => Expr::atom(format!("{:?}", rng.text(0, 8))),
            1 => Expr::atom(format!("{:?}", rng.text(0, 95))),
            _ => Expr::atom(format!("Path::{}", rng.text(1, 70))),
        }
    }

    fn random_atoms(rng: &mut Lcg) -> Vec<Expr> {
        let count = rng.below(16);
        let short = rng.below(2) == 0;
        (0..count)
            .map(|_| {
                if short {
                    Expr::atom(format!("{:?}", rng.text(0, 8)))
                } else {
                    random_atom(rng)
                }
            })
            .collect()
    }

    fn random_value(rng: &mut Lcg, depth: usize) -> Expr {
        match rng.below(if depth < 2 { 7 } else { 6 }) {
            0 => random_atom(rng),
            1 => Expr::slice(random_atoms(rng)),
            2 => Expr::option(Some(random_atom(rng))),
            3 => Expr::option(Some(Expr::Tuple(vec![random_atom(rng), random_atom(rng)]))),
            4 => Expr::Call("Mode::Field".to_owned(), vec![random_atom(rng)]),
            5 => Expr::atom("None"),
            _ => random_struct(rng, depth + 1),
        }
    }

    fn random_struct(rng: &mut Lcg, depth: usize) -> Expr {
        let fields = (0..1 + rng.below(6))
            .map(|index| {
                (
                    format!("f{index}_{}", rng.text(0, 28)),
                    random_value(rng, depth),
                )
            })
            .collect();
        Expr::Struct(format!("Spec{}", rng.text(0, 30)), fields)
    }

    fn random_item_value(rng: &mut Lcg) -> Expr {
        match rng.below(4) {
            0 => Expr::slice(random_atoms(rng)),
            1 => Expr::slice(
                (0..1 + rng.below(3))
                    .map(|_| random_struct(rng, 0))
                    .collect(),
            ),
            2 => random_struct(rng, 0),
            _ => Expr::slice(
                (0..rng.below(8))
                    .map(|_| Expr::atom(format!("CONSTANT_{}", rng.text(1, 40))))
                    .collect(),
            ),
        }
    }

    /// Every layout `item` produces is one rustfmt leaves as it is.
    #[test]
    fn rustfmt_leaves_every_item_layout_alone() {
        let mut rng = Lcg(0x5eed_1a70);
        let mut source = String::new();
        for index in 0..600 {
            let name = format!("ITEM_{index}_{}", rng.text(0, 40));
            let value = random_item_value(&mut rng);
            source.push_str(&item(&format!("pub static {name}: &[Spec] ="), &value));
            source.push_str("\n\n");
        }
        source.pop();
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("items.rs");
        std::fs::write(&path, &source).expect("written");
        let output = std::process::Command::new("rustfmt")
            .args(["--check", "--edition", "2021"])
            .arg(&path)
            .output()
            .expect("rustfmt runs");
        assert!(
            output.status.success(),
            "rustfmt would change these layouts:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
