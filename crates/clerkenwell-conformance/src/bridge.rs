//! The Node process holding TypeScript replicas: `bridge.ts`, written into
//! the npm project whose `@clerkenwell/client` it drives and run with that
//! project's `tsx`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use crate::Bindings;

/// The bridge's source.
const SOURCE: &str = include_str!("bridge.ts");

/// The `@clerkenwell/client` version this crate drives: its own.
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where in an npm project the bridge is written.
const SCRIPT_DIRECTORY: &str = "node_modules/.cache/clerkenwell-conformance";

/// A running bridge. Dropping it stops the process and removes its script.
pub struct Bridge {
    child: Child,
    io: RefCell<(ChildStdin, BufReader<ChildStdout>)>,
    replicas: Cell<u64>,
    script: PathBuf,
}

/// A replica the bridge holds.
pub struct TypeScriptReplica<'a> {
    bridge: &'a Bridge,
    name: String,
}

impl Bridge {
    /// Starts the bridge in `project`, an npm project in which Node resolves
    /// `tsx` and `@clerkenwell/client`, over the TypeScript plans and fixture
    /// corpus of `bindings`. Node resolves packages under `conditions`. A
    /// client of another version than this crate's is refused.
    pub fn start(bindings: &Bindings, project: &Path, conditions: &[&str]) -> Self {
        let project = absolute(project);
        let directory = project.join(SCRIPT_DIRECTORY);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", directory.display()));
        require_packages(&project, &directory, conditions);
        let script = write_script(&directory);
        let mut child = Command::new("node")
            .args(condition_arguments(conditions))
            .args(["--import", "tsx"])
            .arg(&script)
            .arg("--plans")
            .arg(absolute(&bindings.typescript_plans))
            .arg("--fixtures")
            .arg(absolute(&bindings.collaboration_fixtures))
            .current_dir(&project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|error| panic!("the conformance bridge starts under node: {error}"));
        let stdin = child.stdin.take().expect("bridge stdin");
        let stdout = BufReader::new(child.stdout.take().expect("bridge stdout"));
        let bridge = Self {
            child,
            io: RefCell::new((stdin, stdout)),
            replicas: Cell::new(0),
            script,
        };
        let installed = bridge.call(json!({ "op": "clientVersion" }));
        let installed = installed
            .as_str()
            .expect("the bridge answers the client's version");
        assert!(
            installed == CLIENT_VERSION,
            "{} has @clerkenwell/client {installed}; clerkenwell-conformance {CLIENT_VERSION} drives the client of its own version",
            project.display()
        );
        bridge
    }

    /// Sends one request and returns its value, or the bridge's refusal.
    pub fn request(&self, request: &Value) -> Result<Value, String> {
        let mut io = self.io.borrow_mut();
        let (stdin, stdout) = &mut *io;
        writeln!(stdin, "{request}")
            .and_then(|()| stdin.flush())
            .unwrap_or_else(|error| panic!("the bridge accepts a request: {error}"));
        let mut line = String::new();
        let read = stdout
            .read_line(&mut line)
            .unwrap_or_else(|error| panic!("the bridge answers: {error}"));
        assert!(read > 0, "the bridge exited while answering {request}");
        let response: Value = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("the bridge answers JSON: {error}: {line}"));
        if response["ok"] == true {
            Ok(response["value"].clone())
        } else {
            Err(response["error"].as_str().unwrap_or("").to_string())
        }
    }

    fn call(&self, request: Value) -> Value {
        self.request(&request)
            .unwrap_or_else(|error| panic!("bridge {}: {error}", request["op"]))
    }

    fn name(&self) -> String {
        let next = self.replicas.get() + 1;
        self.replicas.set(next);
        format!("replica-{next}")
    }

    /// The field paths TypeScript reports an edit from `base` to `client`
    /// conflicting on against their policy once the accepted document is
    /// `current`. Documents are in client naming.
    pub fn policy_conflicts(
        &self,
        entity: &str,
        base: &Value,
        client: &Value,
        current: &Value,
    ) -> Vec<String> {
        let paths = self.call(json!({
            "op": "policyConflicts",
            "entity": entity,
            "base": base,
            "client": client,
            "current": current,
        }));
        serde_json::from_value(paths).expect("the bridge answers a list of paths")
    }

    /// A replica seeded from the fixture corpus's client document for `entity`.
    pub fn seed_fixture(&self, entity: &str) -> TypeScriptReplica<'_> {
        let name = self.name();
        self.call(json!({ "op": "seedFixture", "replica": name, "entity": entity }));
        TypeScriptReplica { bridge: self, name }
    }

    /// A replica holding the history `update_base64` carries, or the refusal
    /// of `schema_version`.
    pub fn hydrate(
        &self,
        entity: &str,
        schema_version: u32,
        update_base64: &str,
    ) -> Result<TypeScriptReplica<'_>, String> {
        let name = self.name();
        self.request(&json!({
            "op": "hydrate",
            "replica": name,
            "entity": entity,
            "schemaVersion": schema_version,
            "updateBase64": update_base64,
        }))?;
        Ok(TypeScriptReplica { bridge: self, name })
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.script);
    }
}

/// `path`, absolute against this process's working directory, since the
/// bridge runs in another.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path)
        .unwrap_or_else(|error| panic!("cannot make {} absolute: {error}", path.display()))
}

fn condition_arguments<'a>(conditions: &'a [&str]) -> impl Iterator<Item = String> + 'a {
    conditions
        .iter()
        .map(|condition| format!("--conditions={condition}"))
}

/// Refuses `project` unless Node, resolving from `directory` where the bridge
/// is written, finds `tsx` and `@clerkenwell/client`: in the project's own
/// `node_modules` or one above it.
fn require_packages(project: &Path, directory: &Path, conditions: &[&str]) {
    const RESOLVE: &str = "for (const name of ['tsx', '@clerkenwell/client']) { \
        try { import.meta.resolve(name); } catch { console.log(name); } }";
    let output = Command::new("node")
        .args(condition_arguments(conditions))
        .args(["--input-type=module", "--eval", RESOLVE])
        .current_dir(directory)
        .output()
        .unwrap_or_else(|error| panic!("node runs: {error}"));
    assert!(
        output.status.success(),
        "node resolves packages: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let missing = String::from_utf8_lossy(&output.stdout);
    let missing: Vec<&str> = missing.lines().collect();
    assert!(
        missing.is_empty(),
        "the conformance bridge needs {} installed in {} or a project above it",
        missing.join(" and "),
        project.display()
    );
}

/// Writes a copy of the bridge into `directory`, inside the project, where
/// Node resolves packages from that project. Each bridge has its own copy, an
/// ES module whatever the project's module type.
fn write_script(directory: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let script = directory.join(format!(
        "bridge-{}-{}.mts",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&script, SOURCE)
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", script.display()));
    script
}

impl TypeScriptReplica<'_> {
    fn call(&self, op: &str, mut fields: Value) -> Value {
        fields["op"] = json!(op);
        fields["replica"] = json!(self.name);
        self.bridge.call(fields)
    }

    /// Writes `client_document` as the operations between it and the
    /// replica's current document.
    pub fn replace(&self, client_document: &Value) {
        self.call("replace", json!({ "document": client_document }));
    }

    pub fn import(&self, schema_version: u32, update_base64: &str) -> Result<(), String> {
        self.bridge
            .request(&json!({
                "op": "import",
                "replica": self.name,
                "schemaVersion": schema_version,
                "updateBase64": update_base64,
            }))
            .map(|_| ())
    }

    pub fn export(&self) -> String {
        string(self.call("export", json!({})))
    }

    pub fn export_incremental(&self, frontier_base64: &str) -> String {
        string(self.call(
            "exportIncremental",
            json!({ "frontierBase64": frontier_base64 }),
        ))
    }

    pub fn frontier(&self) -> String {
        string(self.call("frontier", json!({})))
    }

    /// The replica's document in client naming, read at `revision`.
    pub fn materialize(&self, revision: &str) -> Value {
        self.call("materialize", json!({ "revision": revision }))
    }

    /// The text a declared field's binding reads and the replica's frontier.
    pub fn capture_text(
        &self,
        field: &str,
        identities: &HashMap<String, String>,
    ) -> (String, String) {
        let captured = self.call(
            "captureText",
            json!({ "field": field, "identities": identities }),
        );
        (
            string(captured["text"].clone()),
            string(captured["frontierBase64"].clone()),
        )
    }

    pub fn read_text(&self, field: &str, identities: &HashMap<String, String>) -> String {
        string(self.call(
            "readText",
            json!({ "field": field, "identities": identities }),
        ))
    }

    /// Types `text` through the field's text binding at a UTF-16 offset.
    pub fn insert_text(
        &self,
        field: &str,
        identities: &HashMap<String, String>,
        utf16_offset: usize,
        text: &str,
    ) {
        self.call(
            "insertText",
            json!({
                "field": field,
                "identities": identities,
                "offset": utf16_offset,
                "text": text,
            }),
        );
    }
}

fn string(value: Value) -> String {
    match value {
        Value::String(value) => value,
        other => panic!("the bridge answered {other}, not a string"),
    }
}
