//! The Node process holding TypeScript replicas: `conformance/bridge.ts`,
//! run with the npm workspace's `tsx`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

use crate::Bindings;

/// A running bridge. Dropping it stops the process.
pub struct Bridge {
    child: Child,
    io: RefCell<(ChildStdin, BufReader<ChildStdout>)>,
    replicas: Cell<u64>,
}

/// A replica the bridge holds.
pub struct TypeScriptReplica<'a> {
    bridge: &'a Bridge,
    name: String,
}

impl Bridge {
    /// Starts `conformance/bridge.ts` from `workspace`, a Clerkenwell checkout
    /// whose npm workspace is installed, over the TypeScript plans and fixture
    /// corpus of `bindings`.
    pub fn start(bindings: &Bindings, workspace: &Path) -> Self {
        assert!(
            workspace.join("node_modules/tsx").is_dir(),
            "the conformance bridge needs the npm workspace at {}: run `npm ci` there",
            workspace.display()
        );
        let mut child = Command::new("node")
            .args(["--import", "tsx", "conformance/bridge.ts", "--plans"])
            .arg(&bindings.typescript_plans)
            .arg("--fixtures")
            .arg(&bindings.collaboration_fixtures)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|error| panic!("the conformance bridge starts under node: {error}"));
        let stdin = child.stdin.take().expect("bridge stdin");
        let stdout = BufReader::new(child.stdout.take().expect("bridge stdout"));
        Self {
            child,
            io: RefCell::new((stdin, stdout)),
            replicas: Cell::new(0),
        }
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
    }
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
