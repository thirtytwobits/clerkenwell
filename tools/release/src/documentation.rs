//! The site of the workspace's documentation.
//!
//! `cargo doc --workspace --no-deps` writes each library's API documentation
//! to its own directory and no page linking them. [`index_documentation`]
//! adds the rest of the site once it has checked that every library is
//! documented and that the API documentation names no Loro item or type: the
//! README as the front page, followed by the libraries, and each Markdown
//! document the README reaches by its links. Every page states the version.

use std::collections::{BTreeSet, VecDeque};
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
const LORO_PATH: &[u8] = b"loro::";

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
/// tree that lacks a library's documentation or names Loro, and a document
/// that links a file the workspace lacks.
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
        return Err(Error::NamesLoro(naming));
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
/// content names a Loro path, relative to `doc`.
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
    if named
        || content
            .windows(LORO_PATH.len())
            .any(|window| window == LORO_PATH)
    {
        naming.push(relative);
    }
    Ok(())
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

impl Documents<'_> {
    /// Each page of the site and its HTML: [`FRONT_PAGE`] followed by
    /// `crates`, then every document it reaches by its links.
    fn render(&self, crates: &[DocumentedCrate]) -> Result<Vec<(String, String)>, Error> {
        let mut seen = BTreeSet::from([FRONT_PAGE.to_owned()]);
        let mut queue = VecDeque::from([FRONT_PAGE.to_owned()]);
        let mut pages = Vec::new();
        while let Some(source) = queue.pop_front() {
            let text = read(&self.root.join(&source))?;
            let mut linked = Vec::new();
            let mut title = String::new();
            let mut in_title = false;
            let mut events = Vec::new();
            for event in Parser::new_ext(&text, MARKDOWN) {
                let event = match event {
                    Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }) => Event::Start(Tag::Link {
                        link_type,
                        dest_url: self.link(&source, &dest_url, &mut linked)?.into(),
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
                        dest_url: self.link(&source, &dest_url, &mut linked)?.into(),
                        title,
                        id,
                    }),
                    event => event,
                };
                match &event {
                    Event::Start(Tag::Heading {
                        level: HeadingLevel::H1,
                        ..
                    }) if title.is_empty() => in_title = true,
                    Event::End(TagEnd::Heading(HeadingLevel::H1)) => in_title = false,
                    Event::Text(text) | Event::Code(text) if in_title => title.push_str(text),
                    _ => {}
                }
                events.push(event);
            }
            let mut body = String::new();
            pulldown_cmark::html::push_html(&mut body, events.into_iter());
            if source == FRONT_PAGE {
                body.push_str(&crate_list(crates));
            }
            let page = page_of(&source);
            if title.is_empty() {
                title = source.clone();
            }
            pages.push((page.clone(), self.layout(&title, &page, &body)));
            for target in linked {
                if seen.insert(target.clone()) {
                    queue.push_back(target);
                }
            }
        }
        Ok(pages)
    }

    /// Where `destination`, linked from the document `source`, points in the
    /// site: the page of a Markdown document, which joins `linked`, or the
    /// file at the release's tag in the repository. A URL or a fragment is
    /// kept as written.
    fn link(
        &self,
        source: &str,
        destination: &str,
        linked: &mut Vec<String>,
    ) -> Result<String, Error> {
        if destination.starts_with('#')
            || destination.contains("://")
            || destination.starts_with("mailto:")
        {
            return Ok(destination.to_owned());
        }
        let (path, fragment) = match destination.split_once('#') {
            Some((path, fragment)) => (path, format!("#{fragment}")),
            None => (destination, String::new()),
        };
        let broken = || Error::BrokenLink {
            page: source.to_owned(),
            link: destination.to_owned(),
        };
        let target = resolve(source, path).ok_or_else(broken)?;
        if target.is_empty() || !self.root.join(&target).exists() {
            return Err(broken());
        }
        if target.ends_with(".md") {
            let href = relative(&page_of(source), &page_of(&target));
            linked.push(target);
            return Ok(format!("{href}{fragment}"));
        }
        Ok(format!(
            "{}/blob/v{}/{target}{fragment}",
            self.repository, self.version
        ))
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
<nav><a href=\"{home}\">Clerkenwell {version}</a> | <a href=\"{home}#crates\">Crates</a></nav>
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
    format!("<h2 id=\"crates\">Crates</h2>\n<dl>\n{items}</dl>\n")
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
