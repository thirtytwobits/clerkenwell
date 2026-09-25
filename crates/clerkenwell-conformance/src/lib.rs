//! Cross-language conformance for one definition's collaborative entities.
//!
//! A [`Conformance`] run takes a definition's generated Rust plans, its
//! generated TypeScript plans and its collaboration fixture corpus. It drives
//! `clerkenwell-doc` replicas in this process and `@clerkenwell/client/loro`
//! replicas in a Node bridge, `conformance/bridge.ts`, and requires every
//! exchange to leave both languages holding the same history and
//! materialising the same document.
//!
//! The bridge runs from a Clerkenwell checkout whose npm workspace is
//! installed (`npm ci`).

mod bridge;
mod corpus;
mod documents;

use std::path::{Path, PathBuf};

use clerkenwell_doc::LoroAuthoringDocument;
use clerkenwell_schema::GeneratedCollaborationEntitySpec;
use serde_json::{json, Value};

pub use bridge::{Bridge, TypeScriptReplica};
pub use corpus::{Corpus, Edit, EntityFixture};
pub use documents::{client_document, pointer, pointers, text_targets, TextTarget};

/// The revision every materialisation is read at.
pub const REVISION: &str = "loro:conformance";

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

/// This Clerkenwell checkout, whose npm workspace runs the bridge.
pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the Clerkenwell workspace")
}

/// A Rust replica and a TypeScript replica holding the same seeded history.
struct Pair<'a> {
    rust: LoroAuthoringDocument,
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
    /// Starts a bridge under this checkout's npm workspace over `bindings`.
    pub fn start(bindings: &Bindings) -> Self {
        Self::start_in(bindings, &workspace())
    }

    /// Starts a bridge under `workspace` over `bindings`: an installed npm
    /// workspace whose `tsx` resolves `@clerkenwell/client`, its `/loro`
    /// entry and `loro-crdt`, such as this checkout's or a consumer's.
    pub fn start_in(bindings: &Bindings, workspace: &Path) -> Self {
        let corpus = Corpus::load(&bindings.collaboration_fixtures);
        assert!(
            !corpus.entities().is_empty(),
            "the fixture corpus has entities"
        );
        Self {
            plans: bindings.plans,
            corpus,
            bridge: Bridge::start(bindings, workspace),
        }
    }

    pub fn bridge(&self) -> &Bridge {
        &self.bridge
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
        rust: &LoroAuthoringDocument,
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
        let rust = LoroAuthoringDocument::from_document(plan, &fixture.wire_document)
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
            let expected = LoroAuthoringDocument::from_document(plan, &fixture.wire_document)
                .and_then(|seeded| seeded.materialized_document(REVISION))
                .unwrap_or_else(|error| panic!("{}: Rust seeds: {error}", plan.name));

            let typescript = self.bridge.seed_fixture(plan.name);
            let update = typescript.export();
            let rust = LoroAuthoringDocument::from_versioned_update_base64(
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
                    LoroAuthoringDocument::from_versioned_update_base64(plan, version, &update)
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

        let observer = LoroAuthoringDocument::from_versioned_update_base64(
            plan,
            fixture.schema_version,
            seeded,
        )
        .expect("an observer hydrates");
        for update in [&from_typescript, &from_rust] {
            observer
                .import_versioned_update_base64(fixture.schema_version, update)
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
}
