//! The plan types generated bindings instantiate.
//!
//! `clerkenwell-codegen` turns a definition into `static` values of these
//! types. Projections and mutations are named by their wire names.

/// How an entity's content is authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedAuthoringPolicyKind {
    OptimisticDocument,
    Collaborative,
    CommandOwned,
    ReadOnly,
}

/// How a concurrent edit to an entity is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedAuthoringConflictPolicy {
    ExpectedRevision,
    GeneratedFieldPolicy,
}

/// An entity's authoring policy and the role of each mutation that touches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedEntityAuthoringSpec {
    pub entity: &'static str,
    pub kind: GeneratedAuthoringPolicyKind,
    pub rationale: &'static str,
    pub mutations: &'static [&'static str],
    pub content_mutation: Option<&'static str>,
    pub planning_mutations: &'static [&'static str],
    pub command_mutations: &'static [&'static str],
    pub lifecycle_mutations: &'static [&'static str],
    pub session_mnemonic_key: Option<&'static str>,
    pub conflict_policy: Option<GeneratedAuthoringConflictPolicy>,
}

/// How a client applies a projection's snapshots and patches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedMaterializationPlan {
    KeyedCollection {
        collection_field: &'static str,
        item_field: &'static str,
        item_identity_field: &'static str,
        patch_identity_field: &'static str,
    },
    SequencedText {
        collection_field: &'static str,
        item_identity_field: &'static str,
        patch_identity_field: &'static str,
        snapshot_output_field: &'static str,
        sequence_field: &'static str,
        text_field: &'static str,
        patch_output_field: &'static str,
        delta_text_field: &'static str,
    },
    ReplaceOrRemove {
        snapshot_mode: GeneratedSnapshotMode,
        snapshot_omit_fields: &'static [&'static str],
        remove_mode: GeneratedRemoveMode,
        update_fields: Option<(&'static str, &'static str)>,
    },
    Replace {
        snapshot_mode: GeneratedSnapshotMode,
        snapshot_omit_fields: &'static [&'static str],
    },
    Reset,
}

/// Where a replacing plan finds its value: the whole patch, or one field of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedSnapshotMode {
    Patch,
    Field(&'static str),
}

/// How a removal is signalled: a null snapshot, or a null field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedRemoveMode {
    NullSnapshot,
    NullField(&'static str),
}

/// A projection, the entities it reads, and how clients materialise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedProjectionSpec {
    pub name: &'static str,
    pub depends_on: &'static [&'static str],
    pub materialization: GeneratedMaterializationPlan,
}

/// A mutation and the entities it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedMutationSpec {
    pub name: &'static str,
    pub touches: &'static [&'static str],
}

/// How a collaborative field is held in the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedCollaborationStorageKind {
    Scalar,
    Text,
    OrderedList,
    StructuredList,
    StructuredMap,
    StructuredDocument,
    DerivedIdentity,
    DerivedRevision,
    KeyedSequence,
}

/// How a collaborative field's value is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedCollaborationValueCodec {
    Integer,
    Number,
    OptionalNumber,
    Boolean,
    String,
    OptionalString,
    PropertyText,
    StringList,
    StructuredJson,
    Identity,
    KeyedSequence,
}

/// How a concurrent change to a collaborative field is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedCollaborationConflict {
    Immutable,
    Explicit,
    Merge,
    LastWriterWins,
}

/// One declared field of a collaborative document and where it lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedCollaborationFieldSpec {
    pub path: &'static str,
    pub storage_kind: GeneratedCollaborationStorageKind,
    pub container: Option<&'static str>,
    pub container_template: Option<&'static str>,
    pub key: Option<&'static str>,
    pub identity_path: Option<&'static str>,
    pub identity_variable: Option<&'static str>,
    pub order_container: Option<&'static str>,
    pub item_container_template: Option<&'static str>,
    pub metadata_container: Option<&'static str>,
    pub metadata_container_template: Option<&'static str>,
    pub metadata_key: Option<&'static str>,
    pub codec: GeneratedCollaborationValueCodec,
    pub value_schema: Option<&'static str>,
    pub required: bool,
    pub conflict: GeneratedCollaborationConflict,
}

/// A collaborative entity's document layout and the wire names that carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedCollaborationEntitySpec {
    pub name: &'static str,
    pub id_field: &'static str,
    pub substrate: &'static str,
    pub schema_version: u32,
    pub migration_ids: &'static [&'static str],
    pub authoring_projection: &'static str,
    pub import_mutation: &'static str,
    pub root_container: &'static str,
    pub fields: &'static [GeneratedCollaborationFieldSpec],
}
