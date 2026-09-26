//! A walk-through of Clerkenwell over one collaborative note.
//!
//! The note's definition, `notes.projections.json`, declares a title writers
//! overwrite, a body whose concurrent edits merge, and a status whose
//! concurrent changes must be resolved explicitly. `model` holds the
//! bindings generated from it. Each step of the walk-through is a function
//! here; the binary runs them in order and the tests check each outcome.

pub mod model;

use std::path::Path;

use clerkenwell_doc::{CollaborationLoroError, LoroAuthoringDocument};
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationImportResult, CollaborationRecoveryAuditRecord, CollaborationService, ImportFence,
    StoreError,
};
use model::{GeneratedCollaborationEntitySpec, NoteDocumentStatus};
use serde_json::{json, Value};

/// The note's collaboration plan.
pub const NOTE: &GeneratedCollaborationEntitySpec = &model::NOTE_COLLABORATION_SPEC;

/// The note every step works on.
pub const NOTE_ID: &str = "launch-plan";

const RELATIVE_PATH: &str = "notes/launch-plan.json";

/// Why a step failed.
#[derive(Debug)]
pub enum Error {
    Store(StoreError),
    Replica(CollaborationLoroError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "{error}"),
            Self::Replica(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<CollaborationLoroError> for Error {
    fn from(error: CollaborationLoroError) -> Self {
        Self::Replica(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The note as it is first written.
pub fn seed() -> Value {
    json!({
        "title": "Launch plan",
        "body": "Ship the notes example.",
        "status": "draft",
    })
}

/// The application's check on every document the store accepts: a title,
/// and a status the definition declares.
fn validate(note: &Value) -> std::result::Result<(), StoreError> {
    if note["title"].as_str().is_none_or(str::is_empty) {
        return Err(StoreError::invalid_request("A note needs a title."));
    }
    serde_json::from_value::<NoteDocumentStatus>(note["status"].clone())
        .map(|_| ())
        .map_err(|_| StoreError::invalid_request(format!("{} is not a status.", note["status"])))
}

/// A store holding the note.
pub struct Notes {
    service: CollaborationService,
    note: CollaborationDocumentId,
}

/// A writer's replica of the note, and the accepted frontier and etag its
/// edits are based on.
pub struct Writer {
    replica: LoroAuthoringDocument,
    base_frontier: String,
    read_etag: String,
    document: Value,
}

impl Writer {
    /// A replica seeded from `document` outside any store.
    pub fn offline(document: &Value) -> Result<Self> {
        let replica = LoroAuthoringDocument::from_document(NOTE, document)?;
        Ok(Self {
            base_frontier: replica.accepted_frontier_base64(),
            read_etag: String::new(),
            replica,
            document: document.clone(),
        })
    }

    /// The note as this writer holds it.
    pub fn document(&self) -> &Value {
        &self.document
    }

    /// Rewrites the writer's note; the replica records the operations
    /// between the old and new versions.
    pub fn edit(&mut self, edit: impl FnOnce(&mut Value)) -> Result<()> {
        let mut next = self.document.clone();
        edit(&mut next);
        self.replica.replace_document(&next)?;
        self.document = next;
        Ok(())
    }
}

impl Notes {
    /// The accepted note.
    pub fn read(&self) -> Result<Value> {
        self.service
            .detail(NOTE, &self.note)?
            .ok_or_else(|| StoreError::not_found(format!("No note {NOTE_ID}.")).into())
    }

    /// A writer holding the note as accepted now.
    pub fn writer(&self) -> Result<Writer> {
        let read = self.service.authoring_state(NOTE, &self.note, None)?;
        let replica = LoroAuthoringDocument::from_versioned_update_base64(
            NOTE,
            read.schema_version,
            &read.update_base64,
        )?;
        Ok(Writer {
            document: replica.materialized_document(&read.etag)?,
            base_frontier: read.accepted_frontier_base64,
            read_etag: read.etag,
            replica,
        })
    }

    /// Commits the writer's edits, fenced on the frontier the writer read.
    pub fn submit(&self, writer: &Writer, operation_id: &str) -> Result<CollaborationImportResult> {
        self.submit_fenced(writer, operation_id, ImportFence::Frontier)
    }

    /// Commits the writer's edits only if the note is still the one the
    /// writer read.
    pub fn submit_on_read(
        &self,
        writer: &Writer,
        operation_id: &str,
    ) -> Result<CollaborationImportResult> {
        self.submit_fenced(
            writer,
            operation_id,
            ImportFence::Etag(writer.read_etag.clone()),
        )
    }

    fn submit_fenced(
        &self,
        writer: &Writer,
        operation_id: &str,
        fence: ImportFence,
    ) -> Result<CollaborationImportResult> {
        let update = writer
            .replica
            .export_incremental_update_base64(&writer.base_frontier)?;
        Ok(self.service.import(
            NOTE,
            CollaborationImportRequest {
                document: self.note.clone(),
                relative_path: RELATIVE_PATH.to_string(),
                schema_version: NOTE.schema_version,
                operation_id: operation_id.to_string(),
                exchange_mode: CollaborationExchangeMode::Incremental,
                base_frontier_base64: writer.base_frontier.clone(),
                update_base64: update,
                fence,
            },
            validate,
        )?)
    }

    pub fn service(&self) -> &CollaborationService {
        &self.service
    }
}

/// Step 1: a store under `root` holding the note.
pub fn create_note(root: &Path) -> Result<Notes> {
    let service = CollaborationService::new(root, model::GENERATED_COLLABORATION_SPECS);
    let note = CollaborationDocumentId::new(NOTE.name, NOTE_ID);
    service.bootstrap(NOTE, &note, RELATIVE_PATH, &seed(), validate)?;
    Ok(Notes { service, note })
}

/// Step 2: two writers read the note and edit its body concurrently, and one
/// retitles it. Each commit is fenced on the frontier its writer read, so the
/// second merges with the first instead of replacing it. Returns the accepted
/// note.
pub fn merge_concurrent_prose(notes: &Notes) -> Result<Value> {
    let mut ada = notes.writer()?;
    let mut grace = notes.writer()?;
    ada.edit(|note| note["body"] = json!("Ship the notes example. Announce it on Friday."))?;
    grace.edit(|note| {
        note["body"] = json!("Test it first. Ship the notes example.");
        note["title"] = json!("Launch plan, revised");
    })?;
    notes.submit(&ada, "ada-body")?;
    notes.submit(&grace, "grace-body")?;
    notes.read()
}

/// Step 3: a writer whose replica began outside the store submits edits based
/// on a frontier the store never accepted. The store refuses them and the
/// writer must resynchronise. Returns the refusal.
pub fn refuse_an_unknown_base(notes: &Notes) -> Result<StoreError> {
    let mut stranger = Writer::offline(&seed())?;
    stranger.edit(|note| note["body"] = json!("A body from elsewhere."))?;
    match notes.submit(&stranger, "stranger-body") {
        Err(Error::Store(refusal)) => Ok(refusal),
        Err(other) => Err(other),
        Ok(_) => Err(StoreError::internal("The store accepted an unknown base.").into()),
    }
}

/// Step 4: a writer asks for its edit to land only on the note it read. Another
/// writer commits first, so the store refuses the edit as stale and returns the
/// accepted note with its etag. Returns the refusal.
pub fn refuse_a_superseded_read(notes: &Notes) -> Result<StoreError> {
    let mut careful = notes.writer()?;
    let mut quick = notes.writer()?;
    quick.edit(|note| note["title"] = json!("Launch plan, final"))?;
    careful.edit(|note| note["title"] = json!("Launch plan, checked"))?;
    notes.submit(&quick, "quick-title")?;
    match notes.submit_on_read(&careful, "careful-title") {
        Err(Error::Store(refusal)) => Ok(refusal),
        Err(other) => Err(other),
        Ok(_) => Err(StoreError::internal("The store accepted a superseded read.").into()),
    }
}

/// Step 5: two writers set the explicit status to different values. The
/// first commits; the second is refused as a conflict naming the status.
/// Returns the refused writer and the refusal.
pub fn conflict_on_status(notes: &Notes) -> Result<(Writer, StoreError)> {
    let mut ada = notes.writer()?;
    let mut grace = notes.writer()?;
    ada.edit(|note| note["status"] = json!("review"))?;
    grace.edit(|note| note["status"] = json!("published"))?;
    notes.submit(&ada, "ada-status")?;
    match notes.submit(&grace, "grace-status") {
        Err(Error::Store(refusal)) => Ok((grace, refusal)),
        Err(other) => Err(other),
        Ok(_) => Err(StoreError::internal("The store accepted a conflicting status.").into()),
    }
}

/// Step 6: the refused writer rebases. It reads the accepted note, restates
/// its value for each field the refusal names, and commits. Returns the
/// accepted note.
pub fn rebase(notes: &Notes, refused: &Writer, refusal: &StoreError) -> Result<Value> {
    let paths = refusal
        .data
        .as_ref()
        .and_then(|data| data["conflict_paths"].as_array())
        .ok_or_else(|| StoreError::internal("The refusal names no conflicting fields."))?
        .iter()
        .filter_map(Value::as_str)
        .map(|path| format!("/{}", path.replace('.', "/")))
        .collect::<Vec<_>>();
    let mut rebased = notes.writer()?;
    rebased.edit(|note| {
        for path in &paths {
            if let (Some(target), Some(decided)) =
                (note.pointer_mut(path), refused.document().pointer(path))
            {
                *target = decided.clone();
            }
        }
    })?;
    notes.submit(&rebased, "grace-status-rebased")?;
    notes.read()
}

/// Step 7: an operator exports the note's evidence and reindexes the store.
/// Each recovery request is audited. Returns the audit.
pub fn audit_recovery(notes: &Notes) -> Result<Vec<CollaborationRecoveryAuditRecord>> {
    notes.service.export_for_recovery(&notes.note)?;
    notes.service.reindex()?;
    Ok(notes.service.recovery_audit()?)
}
