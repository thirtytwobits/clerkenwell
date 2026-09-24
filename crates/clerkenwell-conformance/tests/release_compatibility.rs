//! A history saved by loro-crdt 1.13.7 survives Unicode edits exchanged
//! between the installed Rust crate and the WASM package `@clerkenwell/client`
//! installs, which is the npm release `loro-release.json` approves.

use std::io::Write;
use std::process::{Command, Stdio};

use base64::{engine::general_purpose::STANDARD, Engine};
use clerkenwell_conformance::workspace;
use clerkenwell_doc::loro::{cursor::Side, ExportMode, LoroDoc, ToJson};
use serde_json::{json, Value};

fn read_json(path: &str) -> Value {
    let path = workspace().join(path);
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn wasm(request: Value) -> Value {
    let workspace = workspace();
    assert!(
        workspace.join("node_modules/tsx").is_dir(),
        "run `npm ci` in {} first",
        workspace.display()
    );
    let mut child = Command::new("node")
        .args(["--import", "tsx", "conformance/loro-release-bridge.ts"])
        .current_dir(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the installed WASM replica");
    child
        .stdin
        .take()
        .expect("bridge stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write the request");
    let output = child.wait_with_output().expect("the WASM replica answers");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("the WASM replica answers JSON")
}

fn decode(value: &Value) -> Vec<u8> {
    STANDARD
        .decode(value.as_str().expect("base64"))
        .expect("valid base64")
}

#[test]
fn a_saved_1_13_7_history_survives_unicode_edits_between_installed_rust_and_wasm() {
    let fixture = read_json("conformance/fixtures/loro-1.13.7-history.json");
    let approved = read_json("loro-release.json")["npm"]["version"].clone();
    let doc = LoroDoc::from_snapshot(&decode(&fixture["snapshot"])).expect("open the snapshot");
    doc.set_peer_id(202).expect("set the peer");
    assert_eq!(doc.get_deep_value().to_json_value(), fixture["value"]);
    let version_before = doc.oplog_vv();
    doc.import(&decode(&fixture["update"]))
        .expect("import the saved update");
    assert_eq!(
        doc.oplog_vv(),
        version_before,
        "delivering the same history again creates no operations"
    );

    let original = fixture["value"]["text"].as_str().expect("saved text");
    let text = doc.get_text("text");
    let before_emoji = original.find(' ').expect("a space");
    text.delete(before_emoji, 1).expect("delete a space");
    let suffix = " Rust 🦉";
    text.insert(text.len_unicode(), suffix)
        .expect("append in Rust");
    doc.commit();

    let prefix = "JS 🐦 ";
    let remote = wasm(json!({ "snapshot": fixture["snapshot"], "prefix": prefix }));
    assert_eq!(remote["version"], approved, "the loaded WASM package");
    let remote_update = decode(&remote["update"]);
    doc.import(&remote_update).expect("import the WASM edit");
    doc.import(&remote_update)
        .expect("import the WASM edit again");
    let expected = format!(
        "{prefix}{}{}{suffix}",
        &original[..before_emoji],
        &original[before_emoji + 1..]
    );
    assert_eq!(text.to_string(), expected);
    for position in 0..=expected.chars().count() {
        for side in [Side::Left, Side::Middle, Side::Right] {
            let cursor = text
                .get_cursor(position, side)
                .expect("a cursor at every Unicode boundary");
            assert_eq!(
                doc.get_cursor_pos(&cursor).expect("resolve").current.pos,
                position
            );
        }
    }

    let snapshot = doc.export(ExportMode::Snapshot).expect("snapshot");
    let reopened = LoroDoc::from_snapshot(&snapshot).expect("reopen");
    assert_eq!(reopened.get_deep_value(), doc.get_deep_value());
    assert_eq!(reopened.oplog_vv(), doc.oplog_vv());

    let received = wasm(json!({ "snapshot": STANDARD.encode(&snapshot) }));
    assert_eq!(received["value"], doc.get_deep_value().to_json_value());
    let back = LoroDoc::from_snapshot(&decode(&received["snapshot"])).expect("reopen in Rust");
    assert_eq!(back.get_deep_value(), doc.get_deep_value());
    assert_eq!(back.oplog_vv(), doc.oplog_vv());
    assert_eq!(
        doc.get_map("meta")
            .get("title")
            .expect("the saved title")
            .get_deep_value()
            .to_json_value(),
        fixture["value"]["meta"]["title"]
    );
}
