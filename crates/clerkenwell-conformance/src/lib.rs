//! Cross-language conformance for one definition's collaborative entities.
//!
//! A [`Conformance`] run takes a definition's generated Rust plans, its
//! generated TypeScript plans and its collaboration fixture corpus. It drives
//! `clerkenwell-doc` replicas in this process and `@clerkenwell/client/replica`
//! replicas in a Node bridge, and requires every exchange to leave both
//! languages holding the same history and materialising the same document.
//!
//! The bridge runs in an npm project in which Node resolves `tsx` and
//! `@clerkenwell/client` at this crate's version, and drives that installed
//! client.

mod bridge;
mod corpus;
mod documents;

use std::path::{Path, PathBuf};

use clerkenwell_doc::{conflicting_field_paths, CollaborationReplica};
use clerkenwell_schema::{ActorKind, GeneratedCollaborationEntitySpec};
use serde_json::{json, Value};

use bridge::{Bridge, TypeScriptReplica};
use corpus::{Corpus, EntityFixture};
use documents::{client_document, text_targets};

/// The revision every materialisation is read at.
const REVISION: &str = "replica:conformance";

/// The peer a Rust replica seeded from a fixture writes under.
const RUST_PEER: u64 = 1;

/// The peer Rust prepares text consumption under.
const RUST_PREPARING_PEER: u64 = 2;

const WRITER_KINDS: [ActorKind; 4] = [
    ActorKind::Human,
    ActorKind::Agent,
    ActorKind::Service,
    ActorKind::System,
];

/// One definition's generated bindings.
#[derive(Debug, Clone)]
pub struct Bindings {
    /// The generated Rust collaboration plans.
    pub plans: &'static [GeneratedCollaborationEntitySpec],
    /// The generated TypeScript model module, which exports `COLLABORATION_PLANS`.
    pub typescript_plans: PathBuf,
    /// The generated collaboration fixture corpus.
    pub collaboration_fixtures: PathBuf,
}

/// This Clerkenwell checkout, whose npm workspace runs the bridge in
/// Clerkenwell's own tests.
#[cfg(feature = "testing")]
pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the Clerkenwell workspace")
}

/// A Rust replica and a TypeScript replica holding the same seeded history.
struct Pair<'a> {
    rust: CollaborationReplica,
    typescript: TypeScriptReplica<'a>,
    /// The frontier both replicas were seeded at.
    frontier: String,
    /// The seeded history.
    seeded: String,
}

/// A conformance run over one definition's bindings.
pub struct Conformance {
    plans: &'static [GeneratedCollaborationEntitySpec],
    corpus: Corpus,
    bridge: Bridge,
}

/// Requires every fixture's client document to be its wire document in
/// client naming.
pub fn check_client_naming(bindings: &Bindings) {
    let corpus = Corpus::load(&bindings.collaboration_fixtures);
    for fixture in corpus.entities() {
        let plan = plan(bindings.plans, &fixture.entity);
        assert_eq!(
            client_document(plan, &fixture.wire_document),
            fixture.client_document,
            "{} client naming",
            fixture.entity
        );
    }
}

fn plan(
    plans: &'static [GeneratedCollaborationEntitySpec],
    entity: &str,
) -> &'static GeneratedCollaborationEntitySpec {
    plans
        .iter()
        .find(|plan| plan.name == entity)
        .unwrap_or_else(|| panic!("the plans declare the corpus entity {entity}"))
}

impl Conformance {
    /// Starts a bridge in this checkout's npm workspace over `bindings`,
    /// driving the client's TypeScript source.
    #[cfg(feature = "testing")]
    pub fn start(bindings: &Bindings) -> Self {
        Self::launch(bindings, &workspace(), &["@clerkenwell/source"])
    }

    /// Starts a bridge in `project` over `bindings`: an npm project in which
    /// Node resolves `tsx` and `@clerkenwell/client` at this crate's version,
    /// from its own `node_modules` or a workspace's above it. The bridge
    /// drives the client as the project installed it. Relative paths are
    /// taken against this process's working directory.
    pub fn start_in(bindings: &Bindings, project: &Path) -> Self {
        Self::launch(bindings, project, &[])
    }

    fn launch(bindings: &Bindings, project: &Path, conditions: &[&str]) -> Self {
        let corpus = Corpus::load(&bindings.collaboration_fixtures);
        assert!(
            !corpus.entities().is_empty(),
            "the fixture corpus has entities"
        );
        Self {
            plans: bindings.plans,
            corpus,
            bridge: Bridge::start(bindings, project, conditions),
        }
    }

    fn fixtures(
        &self,
    ) -> impl Iterator<Item = (&'static GeneratedCollaborationEntitySpec, &EntityFixture)> {
        self.corpus
            .entities()
            .iter()
            .map(|fixture| (plan(self.plans, &fixture.entity), fixture))
    }

    /// Requires both replicas to hold the same history and materialise the
    /// same document.
    fn assert_converged(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        context: &str,
        rust: &CollaborationReplica,
        typescript: &TypeScriptReplica<'_>,
    ) {
        let rust_frontier = rust.accepted_frontier_base64();
        let typescript_frontier = typescript.frontier();
        let includes = |capture: &str, required: &str| {
            rust.frontier_includes(capture, required)
                .unwrap_or_else(|error| panic!("{context}: {error}"))
        };
        assert!(
            includes(&rust_frontier, &typescript_frontier)
                && includes(&typescript_frontier, &rust_frontier),
            "{context}: the replicas hold different histories"
        );
        let rust_document = rust
            .materialized_document(REVISION)
            .unwrap_or_else(|error| panic!("{context}: Rust materialises: {error}"));
        assert_eq!(
            typescript.materialize(REVISION),
            client_document(plan, &rust_document),
            "{context}: TypeScript and Rust materialise different documents"
        );
    }

    /// A Rust replica seeded with the fixture's wire document and a
    /// TypeScript replica hydrated from its history.
    fn pair(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        fixture: &EntityFixture,
    ) -> Pair<'_> {
        let rust = CollaborationReplica::from_document(plan, &fixture.wire_document, RUST_PEER)
            .unwrap_or_else(|error| panic!("{}: Rust seeds: {error}", plan.name));
        let seeded = rust.export_update_base64().expect("Rust exports");
        let typescript = self
            .bridge
            .hydrate(plan.name, fixture.schema_version, &seeded)
            .unwrap_or_else(|error| panic!("{}: TypeScript hydrates: {error}", plan.name));
        Pair {
            frontier: rust.accepted_frontier_base64(),
            rust,
            typescript,
            seeded,
        }
    }

    /// Each language hydrates the history the other seeded from the fixture
    /// document, and both refuse the fixture's unsupported schema versions.
    pub fn seeding(&self) {
        for (plan, fixture) in self.fixtures() {
            let expected =
                CollaborationReplica::from_document(plan, &fixture.wire_document, RUST_PEER)
                    .and_then(|seeded| seeded.materialized_document(REVISION))
                    .unwrap_or_else(|error| panic!("{}: Rust seeds: {error}", plan.name));

            let typescript = self.bridge.seed_fixture(plan.name);
            let update = typescript.export();
            let rust = CollaborationReplica::from_versioned_update_base64(
                plan,
                fixture.schema_version,
                &update,
            )
            .unwrap_or_else(|error| panic!("{}: Rust hydrates: {error}", plan.name));
            let context = format!("{} seeded in TypeScript", plan.name);
            assert_eq!(
                rust.materialized_document(REVISION).expect("materialise"),
                expected,
                "{context}"
            );
            self.assert_converged(plan, &context, &rust, &typescript);

            let Pair {
                rust, typescript, ..
            } = self.pair(plan, fixture);
            self.assert_converged(
                plan,
                &format!("{} seeded in Rust", plan.name),
                &rust,
                &typescript,
            );

            for version in fixture.unsupported_schema_versions() {
                assert!(
                    self.bridge.hydrate(plan.name, version, &update).is_err(),
                    "{}: TypeScript refuses schema version {version}",
                    plan.name
                );
                assert!(
                    typescript.import(version, &update).is_err(),
                    "{}: TypeScript refuses an update at schema version {version}",
                    plan.name
                );
                assert!(
                    CollaborationReplica::from_versioned_update_base64(plan, version, &update)
                        .is_err(),
                    "{}: Rust refuses schema version {version}",
                    plan.name
                );
            }
        }
    }

    /// Applies the fixture's edits one after another, alternating which
    /// language writes each, and requires the other to read it back after an
    /// incremental exchange. Runs once starting in each language.
    pub fn fixture_operations(&self) {
        for (plan, fixture) in self.fixtures() {
            for rust_first in [true, false] {
                let Pair {
                    mut rust,
                    typescript,
                    ..
                } = self.pair(plan, fixture);
                let mut document = fixture.wire_document.clone();
                for (index, edit) in fixture.edits().iter().enumerate() {
                    let edited = edit.apply(&document);
                    let frontier = rust.accepted_frontier_base64();
                    let context = if (index % 2 == 0) == rust_first {
                        rust.replace_document(&edited).unwrap_or_else(|error| {
                            panic!("{}: Rust writes {}: {error}", plan.name, edit.name)
                        });
                        let update = rust
                            .export_incremental_update_base64(&frontier)
                            .expect("Rust exports");
                        typescript
                            .import(fixture.schema_version, &update)
                            .unwrap_or_else(|error| {
                                panic!("{}: TypeScript imports {}: {error}", plan.name, edit.name)
                            });
                        format!("{} {} written in Rust", plan.name, edit.name)
                    } else {
                        typescript.replace(&client_document(plan, &edited));
                        let update = typescript.export_incremental(&frontier);
                        rust.adopt_versioned_update_base64(fixture.schema_version, &update)
                            .unwrap_or_else(|error| {
                                panic!("{}: Rust imports {}: {error}", plan.name, edit.name)
                            });
                        format!("{} {} written in TypeScript", plan.name, edit.name)
                    };
                    self.assert_converged(plan, &context, &rust, &typescript);
                    document = edited;
                }
            }
        }
    }

    /// For every pair of fixture edits that change the fixture document, Rust
    /// writes one and TypeScript the other from the same base; each receives
    /// the other's incremental update, and a third replica receiving both in
    /// the opposite order agrees with them.
    pub fn concurrent_edits(&self) {
        for (plan, fixture) in self.fixtures() {
            let base = &fixture.wire_document;
            let edits = fixture
                .edits()
                .into_iter()
                .filter(|edit| edit.apply(base) != *base)
                .collect::<Vec<_>>();
            for rust_edit in &edits {
                for typescript_edit in &edits {
                    if rust_edit.name == typescript_edit.name {
                        continue;
                    }
                    let context = format!(
                        "{}: Rust {} with TypeScript {}",
                        plan.name, rust_edit.name, typescript_edit.name
                    );
                    let mut pair = self.pair(plan, fixture);
                    pair.rust
                        .replace_document(&rust_edit.apply(base))
                        .unwrap_or_else(|error| panic!("{context}: Rust writes: {error}"));
                    pair.typescript
                        .replace(&client_document(plan, &typescript_edit.apply(base)));
                    self.exchange(plan, fixture, &context, &mut pair);
                }
            }
        }
    }

    /// Exchanges the updates each replica of `pair` wrote since it was seeded
    /// and requires both, and a replica of the seeded history receiving them
    /// in the opposite order, to converge.
    fn exchange(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        fixture: &EntityFixture,
        context: &str,
        pair: &mut Pair<'_>,
    ) {
        let Pair {
            rust,
            typescript,
            frontier,
            seeded,
        } = pair;
        let from_rust = rust
            .export_incremental_update_base64(frontier)
            .expect("Rust exports");
        let from_typescript = typescript.export_incremental(frontier);
        typescript
            .import(fixture.schema_version, &from_rust)
            .unwrap_or_else(|error| panic!("{context}: TypeScript imports: {error}"));
        rust.adopt_versioned_update_base64(fixture.schema_version, &from_typescript)
            .unwrap_or_else(|error| panic!("{context}: Rust imports: {error}"));
        self.assert_converged(plan, context, rust, typescript);

        let mut observer = CollaborationReplica::from_versioned_update_base64(
            plan,
            fixture.schema_version,
            seeded,
        )
        .expect("an observer hydrates");
        for update in [&from_typescript, &from_rust] {
            observer
                .adopt_versioned_update_base64(fixture.schema_version, update)
                .unwrap_or_else(|error| panic!("{context}: the observer imports: {error}"));
        }
        assert_eq!(
            observer
                .materialized_document(REVISION)
                .expect("materialise"),
            rust.materialized_document(REVISION).expect("materialise"),
            "{context}: the opposite import order converges"
        );
    }

    /// Rust appends to every text field of the fixture document while
    /// TypeScript types at the start of each through its text binding. Both
    /// insertions survive in every field.
    pub fn concurrent_typing(&self) {
        let (rust_suffix, typescript_prefix) = (" (Rust 🦀)", "(TypeScript 👩‍💻) ");
        for (plan, fixture) in self.fixtures() {
            let base = &fixture.wire_document;
            let targets = text_targets(plan, base);
            let mut pair = self.pair(plan, fixture);
            let mut appended = base.clone();
            for target in &targets {
                let text = base
                    .pointer(&target.pointer)
                    .and_then(Value::as_str)
                    .expect("text");
                documents::set(
                    &mut appended,
                    &target.pointer,
                    json!(format!("{text}{rust_suffix}")),
                );
                pair.typescript
                    .insert_text(target.field, &target.identities, 0, typescript_prefix);
            }
            pair.rust
                .replace_document(&appended)
                .unwrap_or_else(|error| panic!("{}: Rust appends: {error}", plan.name));
            let context = format!("{} concurrent typing", plan.name);
            self.exchange(plan, fixture, &context, &mut pair);

            let merged = pair
                .rust
                .materialized_document(REVISION)
                .expect("materialise");
            for target in &targets {
                let original = base
                    .pointer(&target.pointer)
                    .and_then(Value::as_str)
                    .expect("text");
                assert_eq!(
                    merged.pointer(&target.pointer).and_then(Value::as_str),
                    Some(format!("{typescript_prefix}{original}{rust_suffix}").as_str()),
                    "{context}: {} at {:?}",
                    target.field,
                    target.identities
                );
            }
        }
    }

    /// TypeScript captures a text field's binding and keeps typing while Rust
    /// consumes exactly the captured text at the captured frontier. The typing
    /// before and after the consumption survives in both languages.
    pub fn text_consumption(&self) {
        let (first, second) = ("🐈 First\n", "👩‍💻 Second\n");
        for (plan, fixture) in self.fixtures() {
            for target in text_targets(plan, &fixture.wire_document) {
                let context = format!("{} {} at {:?}", plan.name, target.field, target.identities);
                let Pair {
                    rust: mut authority,
                    typescript,
                    ..
                } = self.pair(plan, fixture);
                let (captured, captured_frontier) =
                    typescript.capture_text(target.field, &target.identities);
                typescript.insert_text(target.field, &target.identities, 0, first);
                authority
                    .adopt_versioned_update_base64(fixture.schema_version, &typescript.export())
                    .unwrap_or_else(|error| panic!("{context}: Rust accepts typing: {error}"));
                let consumption = authority
                    .prepare_text_replacement_at_frontier(
                        target.field,
                        &target.identities,
                        &captured_frontier,
                        &captured,
                        "",
                        RUST_PREPARING_PEER,
                    )
                    .unwrap_or_else(|error| panic!("{context}: Rust prepares: {error}"));
                typescript.insert_text(target.field, &target.identities, 0, second);
                for _ in 0..2 {
                    typescript
                        .import(fixture.schema_version, &consumption)
                        .unwrap_or_else(|error| panic!("{context}: TypeScript imports: {error}"));
                }
                authority
                    .adopt_versioned_update_base64(fixture.schema_version, &typescript.export())
                    .unwrap_or_else(|error| panic!("{context}: Rust accepts: {error}"));

                let expected = format!("{second}{first}");
                assert_eq!(
                    typescript.read_text(target.field, &target.identities),
                    expected,
                    "{context}: TypeScript"
                );
                assert_eq!(
                    authority
                        .text_at_frontier(
                            target.field,
                            &target.identities,
                            &authority.accepted_frontier_base64(),
                        )
                        .expect("Rust reads"),
                    expected,
                    "{context}: Rust"
                );
                self.assert_converged(plan, &context, &authority, &typescript);
            }
        }
    }

    /// Rust and TypeScript judge every pair of the fixture's edits alike
    /// against each field's conflict policy, one edit as the client's and the
    /// other as the accepted document's. A pair writing two different values
    /// to the fixture's explicit scalar conflicts.
    pub fn conflict_policy(&self) {
        for (plan, fixture) in self.fixtures() {
            let base = &fixture.wire_document;
            let mut edits = fixture.edits();
            let scalar = fixture.scalar_edit(Value::Null).map(|edit| edit.name);
            edits.extend(
                fixture
                    .scalar_edit(json!("divergent-value"))
                    .map(|mut edit| {
                        edit.name = "divergent scalar".to_string();
                        edit
                    }),
            );
            let documents = edits
                .iter()
                .map(|edit| (edit.name.clone(), edit.apply(base)))
                .collect::<Vec<_>>();
            let client_base = client_document(plan, base);
            for (client_name, client) in &documents {
                for (current_name, current) in &documents {
                    for kind in WRITER_KINDS {
                        let rust = conflicting_field_paths(plan, base, client, current, kind);
                        let typescript = self.bridge.policy_conflicts(
                            &fixture.entity,
                            &client_base,
                            &client_document(plan, client),
                            &client_document(plan, current),
                            kind,
                        );
                        assert_eq!(
                            typescript, rust,
                            "{}: {client_name} against {current_name} by {kind:?}",
                            plan.name
                        );
                    }
                    let rust =
                        conflicting_field_paths(plan, base, client, current, ActorKind::Human);
                    if Some(client_name) == scalar.as_ref() && current_name == "divergent scalar" {
                        assert!(
                            !rust.is_empty(),
                            "{}: the divergent scalars conflict",
                            plan.name
                        );
                    }
                }
            }
        }
    }
}
