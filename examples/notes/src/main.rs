//! Runs the walk-through against a store in a temporary directory.

use clerkenwell_example_notes::{
    audit_recovery, conflict_on_status, create_note, merge_concurrent_prose, rebase,
    refuse_a_superseded_read, refuse_an_unknown_base, Result,
};

fn main() -> Result<()> {
    let root = tempfile::tempdir().expect("a temporary directory");
    println!("Store: {}", root.path().display());

    let notes = create_note(root.path())?;
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

    println!("\n7. Recovery requests are audited:");
    for record in audit_recovery(&notes)? {
        println!(
            "   {:?} {}/{} destructive: {}",
            record.action, record.entity, record.resource_id, record.destructive
        );
    }
    Ok(())
}
