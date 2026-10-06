//! Each step of the walk-through does what it says.

use clerkenwell_example_notes::{
    agent, audit, conflict_on_status, create_note, human, merge_concurrent_prose, rebase,
    refuse_a_superseded_read, refuse_an_agents_status, refuse_an_unknown_base, seed, Notes,
    NOTE_ID,
};
use clerkenwell_store::{CollaborationAuditEvent, CollaborationRecoveryAction, StoreErrorKind};
use serde_json::{json, Value};

fn store() -> Notes {
    create_note().expect("create the note")
}

/// The note without its revision, which changes with every commit.
fn content(note: &Value) -> Value {
    let mut content = note.clone();
    content
        .as_object_mut()
        .expect("a note")
        .remove("etag")
        .expect("a revision");
    content
}

fn etag(notes: &Notes) -> Value {
    notes.read().expect("read")["etag"].clone()
}

#[test]
fn a_created_note_reads_back_as_seeded() {
    let notes = store();
    assert_eq!(content(&notes.read().expect("read")), seed());
}

#[test]
fn concurrent_body_edits_merge_and_the_later_title_is_kept() {
    let notes = store();
    let merged = merge_concurrent_prose(&notes).expect("both commits are accepted");
    assert_eq!(
        merged["body"],
        "Test it first. Ship the notes example. Announce it on Friday."
    );
    assert_eq!(merged["title"], "Launch plan, revised");
    assert_eq!(merged["status"], seed()["status"]);
}

#[test]
fn an_import_based_on_a_frontier_the_store_never_accepted_is_refused_and_changes_nothing() {
    let notes = store();
    let before = notes.read().expect("read");
    let resyncs = notes.service().counters().resync_requirements;

    let refusal = refuse_an_unknown_base(&notes).expect("the step runs");

    assert_eq!(refusal.kind, StoreErrorKind::Conflict);
    assert_eq!(
        refusal
            .data
            .as_ref()
            .map(|data| data["draft_retained"].clone()),
        Some(json!(true))
    );
    assert_eq!(notes.read().expect("read"), before);
    assert_eq!(
        notes.service().counters().resync_requirements,
        resyncs + 1,
        "the refusal asks the writer to resynchronise"
    );
}

#[test]
fn an_edit_fenced_on_a_superseded_read_is_refused_with_the_accepted_note() {
    let notes = store();

    let refusal = refuse_a_superseded_read(&notes).expect("the step runs");

    let accepted = notes.read().expect("read");
    assert_eq!(refusal.kind, StoreErrorKind::Conflict);
    let data = refusal.data.as_ref().expect("the refusal carries data");
    assert_eq!(data["conflict_kind"], "collaboration_revision");
    assert_eq!(data["current_etag"], accepted["etag"]);
    assert_ne!(data["expected_etag"], accepted["etag"]);
    assert_eq!(content(&data["current"]), content(&accepted));
    assert_eq!(accepted["title"], "Launch plan, final");
}

#[test]
fn a_concurrent_explicit_status_is_refused_naming_the_status_and_changes_nothing() {
    let notes = store();
    let (_, refusal) = conflict_on_status(&notes).expect("the step runs");

    assert_eq!(refusal.kind, StoreErrorKind::Conflict);
    let data = refusal.data.as_ref().expect("the refusal carries detail");
    assert_eq!(data["conflict_paths"], json!(["status"]));
    let accepted = notes.read().expect("read");
    assert_eq!(accepted["status"], "review", "the first status stands");
    assert_eq!(
        data["current"], accepted,
        "the refusal carries the accepted note to rebase onto"
    );
}

#[test]
fn a_rebased_status_is_accepted_over_the_one_it_conflicted_with() {
    let notes = store();
    let (grace, refusal) = conflict_on_status(&notes).expect("the conflict");
    let before = etag(&notes);

    let rebased = rebase(&notes, &grace, &refusal).expect("the rebased commit is accepted");

    assert_eq!(rebased["status"], grace.document()["status"]);
    assert_ne!(rebased["etag"], before);
    let mut expected = seed();
    expected["status"] = grace.document()["status"].clone();
    assert_eq!(content(&rebased), expected);
}

#[test]
fn an_agents_status_is_refused_unopposed_and_its_body_edit_is_accepted() {
    let notes = store();
    let before = notes.read().expect("read");

    let (refusal, accepted) = refuse_an_agents_status(&notes).expect("the step runs");

    assert_eq!(refusal.kind, StoreErrorKind::Conflict);
    let data = refusal.data.as_ref().expect("the refusal carries detail");
    assert_eq!(data["conflict_paths"], json!(["status"]));
    assert_eq!(accepted["status"], before["status"]);
    assert_ne!(accepted["body"], before["body"]);
}

#[test]
fn every_refusal_and_recovery_request_is_audited_with_who_made_it() {
    let notes = store();
    refuse_an_agents_status(&notes).expect("the agent's refusal");
    let before = notes.read().expect("read");

    let audit = audit(&notes).expect("the step runs");

    let entries: Vec<_> = audit
        .iter()
        .map(|record| {
            let what = match &record.event {
                CollaborationAuditEvent::Refusal { code, .. } => json!(code),
                CollaborationAuditEvent::Recovery {
                    action,
                    destructive,
                    ..
                } => {
                    assert!(!destructive);
                    json!(action)
                }
            };
            (record.actor.clone(), what)
        })
        .collect();
    assert_eq!(
        entries,
        [
            (agent("planner"), json!("collaboration_policy")),
            (
                human("operator"),
                json!(CollaborationRecoveryAction::Export)
            ),
            (
                human("operator"),
                json!(CollaborationRecoveryAction::Reindex)
            ),
        ]
    );
    assert_eq!(audit[0].resource_id, NOTE_ID);
    assert_eq!(notes.read().expect("read"), before);
}

#[test]
fn the_walkthrough_runs_to_completion() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_clerkenwell-example-notes"))
        .output()
        .expect("the example runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
