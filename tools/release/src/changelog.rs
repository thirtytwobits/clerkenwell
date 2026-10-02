//! Release notes: a version's section of `CHANGELOG.md`.

use std::path::Path;

use crate::{read, Error, Version};

/// The file under the workspace root that holds every release's notes.
pub const CHANGELOG: &str = "CHANGELOG.md";

/// The title of a release's heading line, `## <title>`.
fn heading(line: &str) -> Option<&str> {
    line.strip_prefix("## ").map(str::trim)
}

/// The notes for `version`: the body of the section of `CHANGELOG.md` under
/// `root` whose `## ` heading starts with the version, up to the next such
/// heading. A version without a section, or with an empty one, is refused.
pub fn release_notes(root: &Path, version: Version) -> Result<String, Error> {
    let text = read(&root.join(CHANGELOG))?;
    let wanted = version.to_string();
    let mut lines = text.lines();
    lines
        .by_ref()
        .find(|line| {
            heading(line).and_then(|title| title.split_whitespace().next()) == Some(wanted.as_str())
        })
        .ok_or(Error::NoNotes { version })?;
    let body: Vec<&str> = lines.take_while(|line| heading(line).is_none()).collect();
    let notes = body.join("\n");
    let notes = notes.trim();
    if notes.is_empty() {
        return Err(Error::NoNotes { version });
    }
    Ok(format!("{notes}\n"))
}
