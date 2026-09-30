//! The public API of a crate, read from its source.
//!
//! A record lists every item a crate's public paths reach: its signature, the
//! fields and variants it exposes, the public methods and trait
//! implementations of its types, and the crates those signatures name types
//! from. An item behind a `cfg` carries the condition. Items behind
//! `cfg(test)` are not part of the API.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::visit::Visit;
use syn::{Attribute, Fields, ImplItem, Item, Signature, TraitItem, UseTree, Visibility};

/// The crates under `crates/` whose public API is recorded.
pub const RECORDED: &[&str] = &[
    "clerkenwell-axum",
    "clerkenwell-codegen",
    "clerkenwell-conformance",
    "clerkenwell-doc",
    "clerkenwell-events",
    "clerkenwell-schema",
    "clerkenwell-session",
    "clerkenwell-store",
];

/// The crates under `crates/` that hold generated bindings, which
/// `clerkenwell-codegen --check` gates instead.
pub const GENERATED: &[&str] = &["clerkenwell-notebook"];

/// Crates no public signature may name a type from.
pub const FORBIDDEN: &[&str] = &["loro"];

/// The file beside a crate's manifest that holds its record.
pub const RECORD_FILE: &str = "public-api.txt";

/// The command that writes every record.
pub const COMMAND: &str = "cargo run -p clerkenwell-public-api";

/// This Clerkenwell checkout.
pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A crate's public API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The crate's name as Rust paths spell it.
    pub name: String,
    /// One line per item, method, field, variant and trait implementation.
    pub lines: Vec<String>,
    /// The crates the public signatures name types from, other than the
    /// standard library.
    pub names: BTreeSet<String>,
}

impl Record {
    /// The record as its file holds it.
    pub fn text(&self) -> String {
        let names = if self.names.is_empty() {
            "none".to_owned()
        } else {
            self.names.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        let mut text = format!(
            "# The public API of {}. Written by `{COMMAND}`.\n# Types named from: {names}\n\n",
            self.name
        );
        for line in &self.lines {
            text.push_str(line);
            text.push('\n');
        }
        text
    }
}

/// Reads the public API of the library crate whose manifest is in `dir`.
pub fn record(dir: &Path) -> Result<Record, String> {
    let manifest = read(&dir.join("Cargo.toml"))?;
    let name = package_name(&manifest)
        .ok_or_else(|| format!("{} names no package", dir.join("Cargo.toml").display()))?
        .replace('-', "_");
    let mut modules = Vec::new();
    load_file(
        &dir.join("src/lib.rs"),
        dir.join("src"),
        Scope {
            path: Vec::new(),
            reachable: true,
            conditions: Vec::new(),
        },
        &mut modules,
    )?;
    Crate { name, modules }.record()
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package {
            if let Some(value) = line.strip_prefix("name") {
                let value = value.trim_start().strip_prefix('=')?.trim();
                return Some(value.trim_matches('"').to_owned());
            }
        }
    }
    None
}

/// Where a module sits.
#[derive(Clone)]
struct Scope {
    /// The module's path from the crate root.
    path: Vec<String>,
    /// Whether every module from the root to it is `pub`.
    reachable: bool,
    /// The `cfg` conditions from the root to it.
    conditions: Vec<String>,
}

struct Module {
    scope: Scope,
    /// Every item but its child modules, with its own `cfg` conditions.
    items: Vec<(Item, Vec<String>)>,
    children: BTreeSet<String>,
    /// What each name its `use` declarations bring in refers to.
    uses: BTreeMap<String, Vec<String>>,
    /// The modules its glob imports bring in.
    globs: Vec<(Vec<String>, bool, Vec<String>)>,
}

/// The `cfg` conditions on an item, or `None` when it exists only in tests.
fn conditions(attrs: &[Attribute]) -> Option<Vec<String>> {
    let mut conditions = Vec::new();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("cfg")) {
        let condition = attr
            .meta
            .require_list()
            .map(|list| render(&list.tokens))
            .unwrap_or_default();
        if condition == "test" {
            return None;
        }
        conditions.push(format!("cfg({condition})"));
    }
    Some(conditions)
}

fn load_file(
    file: &Path,
    children: PathBuf,
    scope: Scope,
    modules: &mut Vec<Module>,
) -> Result<(), String> {
    let parsed = syn::parse_file(&read(file)?)
        .map_err(|error| format!("cannot parse {}: {error}", file.display()))?;
    add_module(parsed.items, children, scope, modules)
}

fn add_module(
    items: Vec<Item>,
    dir: PathBuf,
    scope: Scope,
    modules: &mut Vec<Module>,
) -> Result<(), String> {
    let mut kept = Vec::new();
    let mut children = BTreeSet::new();
    let mut nested = Vec::new();
    let mut uses = BTreeMap::new();
    let mut globs = Vec::new();
    for item in items {
        let Some(own) = conditions(attributes(&item)) else {
            continue;
        };
        match item {
            Item::Mod(module) => {
                children.insert(module.ident.to_string());
                nested.push((module, own));
            }
            item => {
                if let Item::Use(declaration) = &item {
                    let mut names = Vec::new();
                    let mut globbed = Vec::new();
                    flatten(&declaration.tree, &mut Vec::new(), &mut names, &mut globbed);
                    uses.extend(names);
                    let public = matches!(declaration.vis, Visibility::Public(_));
                    globs.extend(
                        globbed
                            .into_iter()
                            .map(|target| (target, public, own.clone())),
                    );
                }
                kept.push((item, own));
            }
        }
    }
    modules.push(Module {
        scope: scope.clone(),
        items: kept,
        children,
        uses,
        globs,
    });
    for (module, own) in nested {
        let name = module.ident.to_string();
        let mut path = scope.path.clone();
        path.push(name.clone());
        let mut conditions = scope.conditions.clone();
        conditions.extend(own);
        let child = Scope {
            path,
            reachable: scope.reachable && matches!(module.vis, Visibility::Public(_)),
            conditions,
        };
        match module.content {
            Some((_, items)) => add_module(items, dir.join(&name), child, modules)?,
            None => {
                let file = dir.join(format!("{name}.rs"));
                let file = if file.is_file() {
                    file
                } else {
                    dir.join(&name).join("mod.rs")
                };
                load_file(&file, dir.join(&name), child, modules)?;
            }
        }
    }
    Ok(())
}

fn attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// Each name a `use` tree brings in, with the path it refers to, and the
/// paths it imports every name of.
fn flatten(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    names: &mut Vec<(String, Vec<String>)>,
    globs: &mut Vec<Vec<String>>,
) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten(&path.tree, prefix, names, globs);
            prefix.pop();
        }
        UseTree::Name(name) if name.ident == "self" => {
            if let Some(last) = prefix.last() {
                names.push((last.clone(), prefix.clone()));
            }
        }
        UseTree::Name(name) => {
            let mut path = prefix.clone();
            path.push(name.ident.to_string());
            names.push((name.ident.to_string(), path));
        }
        UseTree::Rename(rename) if rename.rename == "_" => {}
        UseTree::Rename(rename) => {
            let mut path = prefix.clone();
            path.push(rename.ident.to_string());
            names.push((rename.rename.to_string(), path));
        }
        UseTree::Glob(_) => globs.push(prefix.clone()),
        UseTree::Group(group) => {
            for tree in &group.items {
                flatten(tree, prefix, names, globs);
            }
        }
    }
}

fn item_name(item: &Item) -> Option<String> {
    Some(
        match item {
            Item::Const(item) => &item.ident,
            Item::Enum(item) => &item.ident,
            Item::Fn(item) => &item.sig.ident,
            Item::Static(item) => &item.ident,
            Item::Struct(item) => &item.ident,
            Item::Trait(item) => &item.ident,
            Item::TraitAlias(item) => &item.ident,
            Item::Type(item) => &item.ident,
            Item::Union(item) => &item.ident,
            _ => return None,
        }
        .to_string(),
    )
}

fn is_public(item: &Item) -> bool {
    let visibility = match item {
        Item::Const(item) => &item.vis,
        Item::Enum(item) => &item.vis,
        Item::Fn(item) => &item.vis,
        Item::Static(item) => &item.vis,
        Item::Struct(item) => &item.vis,
        Item::Trait(item) => &item.vis,
        Item::TraitAlias(item) => &item.vis,
        Item::Type(item) => &item.vis,
        Item::Union(item) => &item.vis,
        Item::Use(item) => &item.vis,
        _ => return false,
    };
    matches!(visibility, Visibility::Public(_))
}

/// What a path refers to.
enum Target {
    /// A path from this crate's root.
    Local(Vec<String>),
    /// A path in another crate.
    External(Vec<String>),
    /// A name no module declares or imports: a prelude item, a primitive or a
    /// generic parameter.
    Unresolved,
}

/// What a path to an item refers to once every re-export is followed.
enum Resolved {
    Item { module: usize, item: usize },
    External(Vec<String>),
    Module,
}

/// A public path and the item it reaches.
struct Export {
    path: String,
    conditions: Vec<String>,
    reaches: Reaches,
}

enum Reaches {
    Item { module: usize, item: usize },
    External(Vec<String>),
}

struct Crate {
    name: String,
    modules: Vec<Module>,
}

impl Crate {
    fn module(&self, path: &[String]) -> Option<(usize, &Module)> {
        self.modules
            .iter()
            .enumerate()
            .find(|(_, module)| module.scope.path == path)
    }

    fn item_named(&self, module: usize, name: &str) -> Option<usize> {
        self.modules[module]
            .items
            .iter()
            .position(|(item, _)| item_name(item).as_deref() == Some(name))
    }

    /// Where `segments`, written in the module at `scope`, refers to.
    fn absolute(&self, scope: &[String], segments: &[String], depth: usize) -> Target {
        let Some((first, rest)) = segments.split_first() else {
            return Target::Local(scope.to_vec());
        };
        if depth > 32 {
            return Target::Unresolved;
        }
        match first.as_str() {
            "crate" => Target::Local(rest.to_vec()),
            "self" => Target::Local([scope, rest].concat()),
            "super" => {
                let parent = &scope[..scope.len().saturating_sub(1)];
                if rest.is_empty() {
                    Target::Local(parent.to_vec())
                } else {
                    self.absolute(parent, rest, depth + 1)
                }
            }
            "Self" => Target::Unresolved,
            _ => {
                if let Some((index, module)) = self.module(scope) {
                    if module.children.contains(first) || self.item_named(index, first).is_some() {
                        return Target::Local([scope, segments].concat());
                    }
                    if let Some(target) = module.uses.get(first) {
                        // `use other_crate;` names the crate itself.
                        if target.as_slice() == [first.clone()] {
                            return Target::External(segments.to_vec());
                        }
                        let full = [target.as_slice(), rest].concat();
                        return self.absolute(scope, &full, depth + 1);
                    }
                    for (glob, _, _) in &module.globs {
                        if let Target::Local(path) = self.absolute(scope, glob, depth + 1) {
                            if let Some((index, _)) = self.module(&path) {
                                if self.item_named(index, first).is_some() {
                                    return Target::Local([path.as_slice(), segments].concat());
                                }
                            }
                        }
                    }
                }
                if rest.is_empty() {
                    Target::Unresolved
                } else {
                    Target::External(segments.to_vec())
                }
            }
        }
    }

    /// The item `segments`, written in the module at `scope`, reaches.
    fn resolve(&self, scope: &[String], segments: &[String], depth: usize) -> Option<Resolved> {
        match self.absolute(scope, segments, depth) {
            Target::External(path) => Some(Resolved::External(path)),
            Target::Unresolved => None,
            Target::Local(path) => {
                if self.module(&path).is_some() {
                    return Some(Resolved::Module);
                }
                let (name, module_path) = path.split_last()?;
                let (module, found) = self.module(module_path)?;
                if let Some(item) = self.item_named(module, name) {
                    return Some(Resolved::Item { module, item });
                }
                let target = found.uses.get(name)?;
                self.resolve(module_path, target, depth + 1)
            }
        }
    }

    fn exports(&self) -> Result<Vec<Export>, String> {
        let mut exports = Vec::new();
        for (index, module) in self.modules.iter().enumerate() {
            if !module.scope.reachable {
                continue;
            }
            let at = |name: &str| {
                let mut path = vec![self.name.clone()];
                path.extend(module.scope.path.iter().cloned());
                path.push(name.to_owned());
                path.join("::")
            };
            for (item_index, (item, own)) in module.items.iter().enumerate() {
                if !is_public(item) {
                    continue;
                }
                let conditions = [module.scope.conditions.as_slice(), own].concat();
                match item {
                    Item::Use(declaration) => {
                        let mut names = Vec::new();
                        flatten(
                            &declaration.tree,
                            &mut Vec::new(),
                            &mut names,
                            &mut Vec::new(),
                        );
                        for (name, target) in names {
                            let reaches = match self.resolve(&module.scope.path, &target, 0) {
                                Some(Resolved::Item { module, item }) => {
                                    Reaches::Item { module, item }
                                }
                                Some(Resolved::External(path)) => Reaches::External(path),
                                Some(Resolved::Module) | None => {
                                    return Err(format!(
                                        "{} re-exports {}, which is not an item",
                                        at(&name),
                                        target.join("::")
                                    ))
                                }
                            };
                            exports.push(Export {
                                path: at(&name),
                                conditions: conditions.clone(),
                                reaches,
                            });
                        }
                    }
                    item => {
                        if let Some(name) = item_name(item) {
                            exports.push(Export {
                                path: at(&name),
                                conditions,
                                reaches: Reaches::Item {
                                    module: index,
                                    item: item_index,
                                },
                            });
                        }
                    }
                }
            }
            for (glob, public, own) in &module.globs {
                if !public {
                    continue;
                }
                let Target::Local(path) = self.absolute(&module.scope.path, glob, 0) else {
                    return Err(format!(
                        "{} re-exports every name of {}",
                        at("*"),
                        glob.join("::")
                    ));
                };
                let (source, found) = self
                    .module(&path)
                    .ok_or_else(|| format!("{} is not a module", glob.join("::")))?;
                for (item_index, (item, _)) in found.items.iter().enumerate() {
                    if let (true, Some(name)) = (is_public(item), item_name(item)) {
                        exports.push(Export {
                            path: at(&name),
                            conditions: [module.scope.conditions.as_slice(), own].concat(),
                            reaches: Reaches::Item {
                                module: source,
                                item: item_index,
                            },
                        });
                    }
                }
            }
        }
        Ok(exports)
    }

    fn record(&self) -> Result<Record, String> {
        let exports = self.exports()?;
        // An item exported at several paths takes its methods and trait
        // implementations under the shortest.
        let mut canonical: BTreeMap<(usize, usize), &Export> = BTreeMap::new();
        for export in &exports {
            if let Reaches::Item { module, item } = export.reaches {
                let chosen = canonical.entry((module, item)).or_insert(export);
                if (export.path.len(), &export.path) < (chosen.path.len(), &chosen.path) {
                    *chosen = export;
                }
            }
        }
        let mut entries = Vec::new();
        let mut names = BTreeSet::new();
        for export in &exports {
            let prefix = tag(&export.conditions);
            match &export.reaches {
                Reaches::External(target) => {
                    names.insert(target[0].clone());
                    entries.push((
                        export.path.clone(),
                        format!("{prefix}use {} = {}", export.path, target.join("::")),
                    ));
                }
                Reaches::Item { module, item } => {
                    let mut lines = Vec::new();
                    let (item, own) = &self.modules[*module].items[*item];
                    let conditions = [export.conditions.as_slice(), own].concat();
                    let prefix = tag(&conditions);
                    let scope = &self.modules[*module].scope.path;
                    self.describe(item, &export.path, &prefix, &mut lines);
                    self.name_types(scope, item, &mut names);
                    entries.extend(lines);
                }
            }
        }
        for module in &self.modules {
            for (item, own) in &module.items {
                let Item::Impl(block) = item else {
                    continue;
                };
                let syn::Type::Path(self_type) = block.self_ty.as_ref() else {
                    continue;
                };
                let segments = segments(&self_type.path);
                let Some(Resolved::Item {
                    module: at,
                    item: which,
                }) = self.resolve(&module.scope.path, &segments, 0)
                else {
                    continue;
                };
                let Some(export) = canonical.get(&(at, which)) else {
                    continue;
                };
                let path = &export.path;
                let conditions =
                    [export.conditions.as_slice(), &module.scope.conditions, own].concat();
                let generics = if block.generics.params.is_empty() {
                    String::new()
                } else {
                    format!("impl{} ", render(&block.generics))
                };
                let arguments = render(&self_type.path.segments.last().unwrap().arguments);
                let bounds = block
                    .generics
                    .where_clause
                    .as_ref()
                    .map(|clause| format!(" {}", render(clause)))
                    .unwrap_or_default();
                let mut visitor = TypeNames {
                    krate: self,
                    scope: &module.scope.path,
                    names: &mut names,
                };
                visitor.visit_generics(&block.generics);
                match &block.trait_ {
                    Some((negative, trait_path, _)) => {
                        visitor.visit_path(trait_path);
                        let negative = if negative.is_some() { "!" } else { "" };
                        entries.push((
                            format!("{path}\u{0}2 {}", render(trait_path)),
                            format!(
                                "{}{generics}impl {negative}{} for {path}{arguments}{bounds}",
                                tag(&conditions),
                                render(trait_path)
                            ),
                        ));
                    }
                    None => {
                        for member in &block.items {
                            match member {
                                ImplItem::Fn(method)
                                    if matches!(method.vis, Visibility::Public(_)) =>
                                {
                                    let Some(own) = crate::conditions(&method.attrs) else {
                                        continue;
                                    };
                                    visitor.visit_signature(&method.sig);
                                    let method_path = format!("{path}::{}", method.sig.ident);
                                    entries.push((
                                        method_path.clone(),
                                        format!(
                                            "{}{generics}{}{bounds}",
                                            tag(&[conditions.as_slice(), &own].concat()),
                                            signature(&method_path, &method.sig)
                                        ),
                                    ));
                                }
                                ImplItem::Const(constant)
                                    if matches!(constant.vis, Visibility::Public(_)) =>
                                {
                                    let Some(own) = crate::conditions(&constant.attrs) else {
                                        continue;
                                    };
                                    visitor.visit_type(&constant.ty);
                                    let constant_path = format!("{path}::{}", constant.ident);
                                    entries.push((
                                        constant_path.clone(),
                                        format!(
                                            "{}{generics}const {constant_path}: {}",
                                            tag(&[conditions.as_slice(), &own].concat()),
                                            render(&constant.ty)
                                        ),
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        entries.sort();
        entries.dedup();
        for standard in ["std", "core", "alloc"] {
            names.remove(standard);
        }
        Ok(Record {
            name: self.name.clone(),
            lines: entries.into_iter().map(|(_, line)| line).collect(),
            names,
        })
    }

    /// The lines that describe `item` at `path`, each with its sort key.
    fn describe(&self, item: &Item, path: &str, prefix: &str, out: &mut Vec<(String, String)>) {
        let mut push = |key: String, line: String| out.push((key, format!("{prefix}{line}")));
        match item {
            Item::Fn(function) => push(path.to_owned(), signature(path, &function.sig)),
            Item::Const(constant) => push(
                path.to_owned(),
                format!("const {path}: {}", render(&constant.ty)),
            ),
            Item::Static(value) => push(
                path.to_owned(),
                format!("static {path}: {}", render(&value.ty)),
            ),
            Item::Type(alias) => push(
                path.to_owned(),
                format!(
                    "type {path}{} = {}",
                    render(&alias.generics),
                    render(&alias.ty)
                ),
            ),
            Item::TraitAlias(alias) => push(
                path.to_owned(),
                format!(
                    "trait {path}{} = {}",
                    render(&alias.generics),
                    render(&alias.bounds)
                ),
            ),
            Item::Struct(structure) => {
                let hidden = structure
                    .fields
                    .iter()
                    .any(|field| !matches!(field.vis, Visibility::Public(_)));
                push(
                    path.to_owned(),
                    format!(
                        "struct {path}{}{}{}",
                        render(&structure.generics),
                        where_clause(&structure.generics),
                        if hidden { " { .. }" } else { "" }
                    ),
                );
                derives(&structure.attrs, path, &mut push);
                for (index, field) in structure.fields.iter().enumerate() {
                    if !matches!(field.vis, Visibility::Public(_)) {
                        continue;
                    }
                    let name = field
                        .ident
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| index.to_string());
                    push(
                        format!("{path}::{name}"),
                        format!("field {path}::{name}: {}", render(&field.ty)),
                    );
                }
            }
            Item::Union(union) => push(
                path.to_owned(),
                format!("union {path}{} {{ .. }}", render(&union.generics)),
            ),
            Item::Enum(enumeration) => {
                push(
                    path.to_owned(),
                    format!(
                        "enum {path}{}{}",
                        render(&enumeration.generics),
                        where_clause(&enumeration.generics)
                    ),
                );
                derives(&enumeration.attrs, path, &mut push);
                for variant in &enumeration.variants {
                    let fields = match &variant.fields {
                        Fields::Unit => String::new(),
                        Fields::Unnamed(fields) => render(fields),
                        Fields::Named(fields) => format!(
                            " {{ {} }}",
                            fields
                                .named
                                .iter()
                                .map(render)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    push(
                        format!("{path}::{}", variant.ident),
                        format!("variant {path}::{}{fields}", variant.ident),
                    );
                }
            }
            Item::Trait(definition) => {
                let supertraits = if definition.supertraits.is_empty() {
                    String::new()
                } else {
                    format!(": {}", render(&definition.supertraits))
                };
                push(
                    path.to_owned(),
                    format!(
                        "trait {path}{}{supertraits}{}",
                        render(&definition.generics),
                        where_clause(&definition.generics)
                    ),
                );
                for member in &definition.items {
                    match member {
                        TraitItem::Fn(method) => {
                            let method_path = format!("{path}::{}", method.sig.ident);
                            let provided = if method.default.is_some() {
                                " { .. }"
                            } else {
                                ""
                            };
                            push(
                                method_path.clone(),
                                format!("{}{provided}", signature(&method_path, &method.sig)),
                            );
                        }
                        TraitItem::Type(associated) => push(
                            format!("{path}::{}", associated.ident),
                            format!(
                                "type {path}::{}{}{}",
                                associated.ident,
                                render(&associated.generics),
                                if associated.bounds.is_empty() {
                                    String::new()
                                } else {
                                    format!(": {}", render(&associated.bounds))
                                }
                            ),
                        ),
                        TraitItem::Const(constant) => push(
                            format!("{path}::{}", constant.ident),
                            format!("const {path}::{}: {}", constant.ident, render(&constant.ty)),
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Adds the crates `item`'s public parts name types from.
    fn name_types(&self, scope: &[String], item: &Item, names: &mut BTreeSet<String>) {
        let mut visitor = TypeNames {
            krate: self,
            scope,
            names,
        };
        match item {
            Item::Fn(function) => visitor.visit_signature(&function.sig),
            Item::Const(constant) => visitor.visit_type(&constant.ty),
            Item::Static(value) => visitor.visit_type(&value.ty),
            Item::Type(alias) => {
                visitor.visit_generics(&alias.generics);
                visitor.visit_type(&alias.ty);
            }
            Item::TraitAlias(alias) => visitor.visit_item_trait_alias(alias),
            Item::Struct(structure) => {
                visitor.visit_generics(&structure.generics);
                for field in &structure.fields {
                    if matches!(field.vis, Visibility::Public(_)) {
                        visitor.visit_type(&field.ty);
                    }
                }
            }
            Item::Union(union) => visitor.visit_generics(&union.generics),
            Item::Enum(enumeration) => {
                visitor.visit_generics(&enumeration.generics);
                for variant in &enumeration.variants {
                    visitor.visit_fields(&variant.fields);
                }
            }
            Item::Trait(definition) => {
                visitor.visit_generics(&definition.generics);
                for bound in &definition.supertraits {
                    visitor.visit_type_param_bound(bound);
                }
                for member in &definition.items {
                    match member {
                        TraitItem::Fn(method) => visitor.visit_signature(&method.sig),
                        TraitItem::Type(associated) => {
                            for bound in &associated.bounds {
                                visitor.visit_type_param_bound(bound);
                            }
                        }
                        TraitItem::Const(constant) => visitor.visit_type(&constant.ty),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn segments(path: &syn::Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

/// Collects the crates the paths it visits name.
struct TypeNames<'a> {
    krate: &'a Crate,
    scope: &'a [String],
    names: &'a mut BTreeSet<String>,
}

impl<'ast> Visit<'ast> for TypeNames<'_> {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let written = segments(path);
        if path.leading_colon.is_some() {
            self.names.insert(written[0].clone());
        } else if let Target::External(target) = self.krate.absolute(self.scope, &written, 0) {
            // A crate's name is snake case; a path from a capitalised name
            // starts at a generic parameter, such as `H::Projection`.
            if target[0].starts_with(|first: char| first.is_ascii_lowercase()) {
                self.names.insert(target[0].clone());
            }
        }
        syn::visit::visit_path(self, path);
    }

    // A function body is not part of its signature.
    fn visit_block(&mut self, _: &'ast syn::Block) {}
}

fn tag(conditions: &[String]) -> String {
    conditions
        .iter()
        .map(|condition| format!("#[{condition}] "))
        .collect()
}

fn derives(attrs: &[Attribute], path: &str, push: &mut impl FnMut(String, String)) {
    let mut derived = BTreeSet::new();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("derive")) {
        let _ = attr.parse_nested_meta(|meta| {
            derived.insert(render(&meta.path));
            Ok(())
        });
    }
    if !derived.is_empty() {
        push(
            format!("{path}\u{0}1"),
            format!(
                "derive {path}: {}",
                derived.into_iter().collect::<Vec<_>>().join(", ")
            ),
        );
    }
}

fn where_clause(generics: &syn::Generics) -> String {
    generics
        .where_clause
        .as_ref()
        .map(|clause| format!(" {}", render(clause)))
        .unwrap_or_default()
}

fn signature(path: &str, signature: &Signature) -> String {
    let mut line = String::new();
    if signature.constness.is_some() {
        line.push_str("const ");
    }
    if signature.asyncness.is_some() {
        line.push_str("async ");
    }
    if signature.unsafety.is_some() {
        line.push_str("unsafe ");
    }
    let inputs = signature
        .inputs
        .iter()
        .map(render)
        .collect::<Vec<_>>()
        .join(", ");
    line.push_str(&format!(
        "fn {path}{}({inputs}){}{}",
        render(&signature.generics),
        match &signature.output {
            syn::ReturnType::Default => String::new(),
            syn::ReturnType::Type(_, output) => format!(" -> {}", render(output)),
        },
        where_clause(&signature.generics)
    ));
    line
}

/// Tokens as source text, spaced the way rustfmt spaces a signature.
fn render(tokens: &impl ToTokens) -> String {
    let stream: TokenStream = tokens.to_token_stream();
    let raw = stream.to_string();
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current == ' ' {
            let previous = out.chars().last().unwrap_or(' ');
            let next = chars.get(index + 1).copied().unwrap_or(' ');
            let after = chars.get(index + 2).copied().unwrap_or(' ');
            let drop = matches!(previous, '&' | '<' | '(' | '[' | '!' | '?' | '#')
                || (previous == ':' && out.ends_with("::"))
                || (previous == '\'' && next.is_alphabetic())
                || matches!(next, ',' | ';' | ')' | ']' | '>' | '?')
                || (next == ':' && after == ':')
                || (next == ':' && after == ' ' && previous != ':')
                || (next == '<'
                    && !out.ends_with("->")
                    && !out.ends_with(" as")
                    && previous != '=')
                || (next == '('
                    && (previous.is_alphanumeric() || previous == '_' || previous == '>')
                    && !out.ends_with("->")
                    && !out.ends_with(" impl")
                    && !out.ends_with(" dyn"));
            if !drop {
                out.push(' ');
            }
        } else {
            out.push(current);
        }
        index += 1;
    }
    out
}
