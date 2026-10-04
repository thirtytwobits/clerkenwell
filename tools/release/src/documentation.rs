//! The site of the workspace's documentation.
//!
//! `cargo doc --workspace --no-deps` writes each library's API documentation
//! to its own directory and no page linking them. [`index_documentation`]
//! adds the rest of the site once it has checked that every library is
//! documented and that the API documentation names no Loro item or type: the
//! README as the front page, followed by the libraries, and each Markdown
//! document the README reaches by its links. Every page states the version.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::{agreed, cargo_metadata, cargo_packages, missing, parse_toml, read, Error, Version};

/// The command that builds the documentation [`index_documentation`] indexes.
pub const DOCUMENTATION_COMMAND: &str = "cargo doc --workspace --no-deps";

/// The Markdown document the site opens with, relative to the workspace.
pub const FRONT_PAGE: &str = "README.md";

/// The directories at the top of a documentation tree that hold the crates'
/// source and rustdoc's assets, which document no item.
const UNDOCUMENTED: &[&str] = &["src", "static.files"];

/// The text rustdoc renders wherever documentation names a Loro path.
const LORO_PATH: &str = "loro::";

/// The prefixes of the anchors rustdoc gives the items it documents inside
/// their parent's page: methods, fields, variants and associated items.
const ITEM_ANCHORS: &[&str] = &[
    "method.",
    "tymethod.",
    "structfield.",
    "variant.",
    "associatedtype.",
    "associatedconstant.",
];

/// The anchor of the front page's list of libraries.
const CRATES_ANCHOR: &str = "crates";

/// The Markdown extensions the documents use.
const MARKDOWN: Options = Options::ENABLE_TABLES.union(Options::ENABLE_STRIKETHROUGH);

/// A workspace library, by the name rustdoc gives its documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentedCrate {
    pub name: String,
    pub description: Option<String>,
}

/// What [`index_documentation`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// The libraries the front page links, in name order.
    pub crates: Vec<DocumentedCrate>,
    /// Each page written, relative to the documentation directory, the front
    /// page first.
    pub pages: Vec<String>,
}

/// Writes the site's pages into `doc`, the API documentation of the
/// workspace under `root`: [`FRONT_PAGE`] as `index.html`, linking each
/// library, and each Markdown document it reaches by its links. Refuses a
/// tree that lacks a library's documentation, a page that names Loro, and a
/// document that links a file the workspace lacks or a heading its target
/// lacks. Nothing is written unless every page is accepted.
pub fn index_documentation(root: &Path, doc: &Path) -> Result<Site, Error> {
    let version = agreed(root)?;
    let mut naming = Vec::new();
    for entry in entries(doc)? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !UNDOCUMENTED.contains(&name.as_str()) {
            find_loro(doc, &entry.path(), &mut naming)?;
        }
    }
    if !naming.is_empty() {
        naming.sort();
        return Err(Error::ForbiddenNames(naming));
    }
    let crates = libraries(root)?;
    let undocumented: Vec<String> = crates
        .iter()
        .filter(|library| !doc.join(&library.name).join("index.html").is_file())
        .map(|library| library.name.clone())
        .collect();
    if !undocumented.is_empty() {
        return Err(Error::Undocumented {
            doc: doc.to_owned(),
            crates: undocumented,
        });
    }
    let documents = Documents {
        root,
        version,
        repository: repository(root)?,
    };
    let rendered = documents.render(&crates)?;
    let mut pages = Vec::with_capacity(rendered.len());
    for (page, html) in rendered {
        let path = doc.join(&page);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Write {
                path: parent.to_owned(),
                source,
            })?;
        }
        std::fs::write(&path, html).map_err(|source| Error::Write { path, source })?;
        pages.push(page);
    }
    Ok(Site { crates, pages })
}

/// Each library target in the workspace under `root`, in name order.
fn libraries(root: &Path) -> Result<Vec<DocumentedCrate>, Error> {
    let metadata = cargo_metadata(root)?;
    let mut crates: Vec<DocumentedCrate> = cargo_packages(&metadata)?
        .iter()
        .flat_map(|package| {
            let description = package["description"].as_str().map(str::to_owned);
            package["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|target| {
                    target["kind"]
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind == "lib"))
                })
                .filter_map(move |target| {
                    Some(DocumentedCrate {
                        name: target["name"].as_str()?.replace('-', "_"),
                        description: description.clone(),
                    })
                })
        })
        .collect();
    crates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(crates)
}

fn entries(directory: &Path) -> Result<Vec<std::fs::DirEntry>, Error> {
    let read = |source| Error::Read {
        path: directory.to_owned(),
        source,
    };
    std::fs::read_dir(directory)
        .map_err(read)?
        .collect::<Result<_, _>>()
        .map_err(read)
}

/// Adds to `naming` each path under `path` that is named after Loro or whose
/// content [`names_loro`], relative to `doc`.
fn find_loro(doc: &Path, path: &Path, naming: &mut Vec<String>) -> Result<(), Error> {
    let relative = path
        .strip_prefix(doc)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let named = path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().to_lowercase().contains("loro"));
    if path.is_dir() {
        if named {
            naming.push(relative);
        }
        for entry in entries(path)? {
            find_loro(doc, &entry.path(), naming)?;
        }
        return Ok(());
    }
    let content = std::fs::read(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    if named || names_loro(&String::from_utf8_lossy(&content)) {
        naming.push(relative);
    }
    Ok(())
}

/// Whether `page` names a Loro path, or documents an item named after Loro
/// inside its parent's page.
fn names_loro(page: &str) -> bool {
    page.contains(LORO_PATH)
        || page.split("id=\"").skip(1).any(|rest| {
            let anchor = rest.split('"').next().unwrap_or_default();
            ITEM_ANCHORS.iter().any(|prefix| anchor.starts_with(prefix))
                && anchor.to_lowercase().contains("loro")
        })
}

/// The repository the workspace's `[workspace.package]` names, which a
/// document's link to a file outside the site points into.
fn repository(root: &Path) -> Result<String, Error> {
    let path = root.join("Cargo.toml");
    let document = parse_toml(&path, &read(&path)?)?;
    document
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("repository"))
        .and_then(|repository| repository.as_str())
        .map(|repository| repository.trim_end_matches('/').to_owned())
        .ok_or_else(|| missing(&path, "workspace.package.repository string"))
}

/// The Markdown documents of one workspace, rendered for one release.
struct Documents<'a> {
    root: &'a Path,
    version: Version,
    repository: String,
}

/// One Markdown document, parsed, with an id on each heading.
struct Document {
    /// Its path, relative to the workspace.
    source: String,
    title: String,
    events: Vec<Event<'static>>,
    /// The fragments a link into its page may name.
    anchors: BTreeSet<String>,
}

/// Where a link in a document points.
enum Target {
    /// A URL, kept as written.
    Url,
    /// A heading in the same document.
    Anchor(String),
    /// Another Markdown document, and a heading in it.
    Document {
        path: String,
        fragment: Option<String>,
    },
    /// Any other file in the workspace, and a place in it.
    File {
        path: String,
        fragment: Option<String>,
    },
}

impl Documents<'_> {
    /// Each page of the site and its HTML: [`FRONT_PAGE`] followed by
    /// `crates`, then every document it reaches by its links. Refuses a
    /// broken link, and a page that names Loro.
    fn render(&self, crates: &[DocumentedCrate]) -> Result<Vec<(String, String)>, Error> {
        let documents = self.load()?;
        let anchors: BTreeMap<&str, &BTreeSet<String>> = documents
            .iter()
            .map(|document| (document.source.as_str(), &document.anchors))
            .collect();
        let mut pages = Vec::with_capacity(documents.len());
        let mut naming = Vec::new();
        for document in &documents {
            let mut events = Vec::with_capacity(document.events.len());
            for event in document.events.iter().cloned() {
                events.push(match event {
                    Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }) => Event::Start(Tag::Link {
                        link_type,
                        dest_url: self.href(document, &dest_url, &anchors)?.into(),
                        title,
                        id,
                    }),
                    Event::Start(Tag::Image {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }) => Event::Start(Tag::Image {
                        link_type,
                        dest_url: self.href(document, &dest_url, &anchors)?.into(),
                        title,
                        id,
                    }),
                    event => event,
                });
            }
            let mut body = String::new();
            pulldown_cmark::html::push_html(&mut body, events.into_iter());
            if document.source == FRONT_PAGE {
                body.push_str(&crate_list(crates));
            }
            let page = page_of(&document.source);
            let html = self.layout(&document.title, &page, &body);
            if names_loro(&html) {
                naming.push(page.clone());
            }
            pages.push((page, html));
        }
        if !naming.is_empty() {
            return Err(Error::ForbiddenNames(naming));
        }
        Ok(pages)
    }

    /// [`FRONT_PAGE`] and every document it reaches by its links, in the
    /// order they are reached.
    fn load(&self) -> Result<Vec<Document>, Error> {
        let mut seen = BTreeSet::from([FRONT_PAGE.to_owned()]);
        let mut queue = VecDeque::from([FRONT_PAGE.to_owned()]);
        let mut documents = Vec::new();
        while let Some(source) = queue.pop_front() {
            let text = read(&self.root.join(&source))?;
            let mut events: Vec<Event<'static>> = Parser::new_ext(&text, MARKDOWN)
                .map(Event::into_static)
                .collect();
            let mut anchors = BTreeSet::new();
            if source == FRONT_PAGE {
                anchors.insert(CRATES_ANCHOR.to_owned());
            }
            let title = name_headings(&mut events, &mut anchors);
            for event in &events {
                if let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) =
                    event
                {
                    if let Target::Document { path, .. } = self.target(&source, dest_url)? {
                        if seen.insert(path.clone()) {
                            queue.push_back(path);
                        }
                    }
                }
            }
            documents.push(Document {
                title: if title.is_empty() {
                    source.clone()
                } else {
                    title
                },
                source,
                events,
                anchors,
            });
        }
        Ok(documents)
    }

    /// What `destination`, linked from the document `source`, points at.
    /// Refuses a path the workspace lacks or that leaves it.
    fn target(&self, source: &str, destination: &str) -> Result<Target, Error> {
        if destination.contains("://") || destination.starts_with("mailto:") {
            return Ok(Target::Url);
        }
        let (path, fragment) = match destination.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment.to_owned())),
            None => (destination, None),
        };
        if path.is_empty() {
            return Ok(Target::Anchor(fragment.unwrap_or_default()));
        }
        let broken = || broken_link(source, destination);
        let path = resolve(source, path).ok_or_else(broken)?;
        if path.is_empty() || !self.root.join(&path).exists() {
            return Err(broken());
        }
        Ok(if path.ends_with(".md") {
            Target::Document { path, fragment }
        } else {
            Target::File { path, fragment }
        })
    }

    /// Where `destination`, linked from `document`, points in the site: the
    /// page of a Markdown document, or the file at the release's tag in the
    /// repository. A URL is kept as written. Refuses a fragment naming a
    /// heading its document lacks; `anchors` holds each document's.
    fn href(
        &self,
        document: &Document,
        destination: &str,
        anchors: &BTreeMap<&str, &BTreeSet<String>>,
    ) -> Result<String, Error> {
        let broken = || broken_link(&document.source, destination);
        Ok(match self.target(&document.source, destination)? {
            Target::Url => destination.to_owned(),
            Target::Anchor(fragment) => {
                if !document.anchors.contains(&fragment) {
                    return Err(broken());
                }
                format!("#{fragment}")
            }
            Target::Document { path, fragment } => {
                let href = relative(&page_of(&document.source), &page_of(&path));
                match fragment {
                    Some(fragment) => {
                        if !anchors
                            .get(path.as_str())
                            .is_some_and(|anchors| anchors.contains(&fragment))
                        {
                            return Err(broken());
                        }
                        format!("{href}#{fragment}")
                    }
                    None => href,
                }
            }
            Target::File { path, fragment } => format!(
                "{}/blob/v{}/{path}{}",
                self.repository,
                self.version,
                fragment
                    .map(|fragment| format!("#{fragment}"))
                    .unwrap_or_default()
            ),
        })
    }

    /// `body` as the page `page` titled `title`.
    fn layout(&self, title: &str, page: &str, body: &str) -> String {
        let version = self.version;
        let title = if page == page_of(FRONT_PAGE) {
            format!("Clerkenwell {version}")
        } else {
            format!("{} - Clerkenwell {version}", escape(title))
        };
        let home = format!("{}index.html", "../".repeat(page.matches('/').count()));
        format!(
            "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>{title}</title>
<style>
body {{ font-family: system-ui, sans-serif; max-width: 52rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.5; }}
nav {{ margin-bottom: 2rem; }}
code, pre {{ font-family: ui-monospace, monospace; }}
pre {{ padding: 0.75rem; overflow-x: auto; background: #f4f4f4; }}
table {{ border-collapse: collapse; }}
th, td {{ border: 1px solid #ccc; padding: 0.25rem 0.5rem; text-align: left; vertical-align: top; }}
dt {{ margin-top: 1rem; font-family: ui-monospace, monospace; }}
dd {{ margin-left: 0; }}
</style>
</head>
<body>
<nav><a href=\"{home}\">Clerkenwell {version}</a> | <a href=\"{home}#{CRATES_ANCHOR}\">Crates</a></nav>
<main>
{body}</main>
</body>
</html>
"
        )
    }
}

/// The front page's list of `crates`, each linking its documentation.
fn crate_list(crates: &[DocumentedCrate]) -> String {
    let items: String = crates
        .iter()
        .map(|library| {
            let name = escape(&library.name);
            let description = library
                .description
                .as_deref()
                .map(|text| format!("<dd>{}</dd>", escape(text)))
                .unwrap_or_default();
            format!("<dt><a href=\"{name}/index.html\">{name}</a></dt>{description}\n")
        })
        .collect();
    format!("<h2 id=\"{CRATES_ANCHOR}\">Crates</h2>\n<dl>\n{items}</dl>\n")
}

/// Gives each heading in `events` an id, as GitHub does, adding each to
/// `anchors`, and returns the text of the first top-level heading.
fn name_headings(events: &mut [Event<'static>], anchors: &mut BTreeSet<String>) -> String {
    let mut title = String::new();
    for index in 0..events.len() {
        let Event::Start(Tag::Heading { level, .. }) = &events[index] else {
            continue;
        };
        let level = *level;
        let mut text = String::new();
        for event in &events[index + 1..] {
            match event {
                Event::End(TagEnd::Heading(_)) => break,
                Event::Text(part) | Event::Code(part) => text.push_str(part),
                _ => {}
            }
        }
        if level == HeadingLevel::H1 && title.is_empty() {
            title = text.clone();
        }
        let anchor = unique(slug(&text), anchors);
        if let Event::Start(Tag::Heading { id, .. }) = &mut events[index] {
            *id = Some(anchor.into());
        }
    }
    title
}

/// `text` as a GitHub heading anchor: lower case, each space a hyphen, and
/// every character but a letter, a digit, a hyphen or an underscore removed.
fn slug(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|character| match character {
            ' ' => Some('-'),
            '-' | '_' => Some(character),
            character if character.is_alphanumeric() => Some(character),
            _ => None,
        })
        .collect()
}

/// `anchor`, or the first of `anchor-1`, `anchor-2` and so on that `anchors`
/// lacks, added to `anchors`.
fn unique(anchor: String, anchors: &mut BTreeSet<String>) -> String {
    let mut candidate = anchor.clone();
    let mut count = 0;
    while !anchors.insert(candidate.clone()) {
        count += 1;
        candidate = format!("{anchor}-{count}");
    }
    candidate
}

fn broken_link(page: &str, link: &str) -> Error {
    Error::BrokenLink {
        page: page.to_owned(),
        link: link.to_owned(),
    }
}

/// The page the document `source` is rendered to: `index.html` for
/// [`FRONT_PAGE`], and its own path with an `.html` extension for any other.
fn page_of(source: &str) -> String {
    if source == FRONT_PAGE {
        return "index.html".to_owned();
    }
    format!("{}.html", source.strip_suffix(".md").unwrap_or(source))
}

/// `path`, linked from the document `source`, relative to the workspace;
/// `None` when it leaves the workspace.
fn resolve(source: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = source.split('/').collect();
    parts.pop();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The page `to` as a link from the page `from`.
fn relative(from: &str, to: &str) -> String {
    let from: Vec<&str> = from.split('/').collect();
    let to: Vec<&str> = to.split('/').collect();
    let directory = &from[..from.len() - 1];
    let shared = directory
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = vec![".."; directory.len() - shared];
    parts.extend(&to[shared..]);
    parts.join("/")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
