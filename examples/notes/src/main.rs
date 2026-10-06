//! Runs the walk-through against a store in memory.

use clerkenwell_example_notes::{
    audit, conflict_on_status, create_note, merge_concurrent_prose, rebase,
    refuse_a_superseded_read, refuse_an_agents_status, refuse_an_unknown_base, Result,
};
use clerkenwell_store::CollaborationAuditEvent;

fn main() -> Result<()> {
    let notes = create_note()?;
    println!("\n1. Created the note:\n{:#}", notes.read()?);

    let merged = merge_concurrent_prose(&notes)?;
    println!("\n2. Ada and Grace edited the body concurrently; both frontier-fenced commits were accepted:\n{merged:#}");

    let refusal = refuse_an_unknown_base(&notes)?;
    println!("\n3. A writer whose replica began outside the store was refused:\n{refusal}");

    let refusal = refuse_a_superseded_read(&notes)?;
    println!("\n4. A writer that fenced its edit on the etag it read was refused, because another writer committed first:\n{refusal}");

    let (grace, refusal) = conflict_on_status(&notes)?;
    println!("\n5. Ada set the status first; Grace's concurrent status was refused:\n{refusal}");

    let rebased = rebase(&notes, &grace, &refusal)?;
    println!("\n6. Grace rebased onto the accepted note and restated her status:\n{rebased:#}");

    let (refusal, accepted) = refuse_an_agents_status(&notes)?;
    println!("\n7. The planner, an agent, may not change the status even unopposed; its body edit was accepted:\n{refusal}\n{accepted:#}");

    println!("\n8. Every refused import and recovery request is audited with who made it:");
    for record in audit(&notes)? {
        let what = match &record.event {
            CollaborationAuditEvent::Refusal {
                operation_id, code, ..
            } => format!("refused {operation_id}: {code}"),
            CollaborationAuditEvent::Recovery { action, .. } => format!("{action:?}"),
        };
        println!(
            "   {} {:?} {}/{} {what}",
            record.actor.id, record.actor.kind, record.entity, record.resource_id
        );
    }
    Ok(())
}
