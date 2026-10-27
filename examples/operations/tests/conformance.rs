//! The tasks' operations give one result through every front end Clapham's
//! harness drives, the reference executable among them.

use clapham_conformance::{run, FrontEnd, InProcess, Process, ProcessFrontEnd, ProcessKind};
use clerkenwell_example_operations::conformance::{fixture, Seed, SEED_VARIABLE};

#[tokio::test]
async fn every_operation_gives_one_result_through_every_front_end() {
    let seed = Seed::mint();
    let kept = tempfile::tempdir().expect("a directory for the seed");
    let path = kept.path().join("seed.json");
    seed.write(&path).expect("the seed is kept");
    let fixture = fixture(seed);
    let mut process = Process::new(env!("CARGO_BIN_EXE_clerkenwell-example-operations"));
    process.env.insert(SEED_VARIABLE.into(), path.into());
    let mut front_ends: Vec<Box<dyn FrontEnd>> = vec![Box::new(InProcess)];
    for kind in [
        ProcessKind::CommandLine,
        ProcessKind::CommandLineStdio,
        ProcessKind::McpStdio,
        ProcessKind::McpHttp,
        ProcessKind::BoundStdio,
        ProcessKind::BoundHttp,
    ] {
        front_ends.push(Box::new(ProcessFrontEnd::new(kind, process.clone())));
    }

    let report = run(
        &fixture,
        std::sync::Arc::new(clapham::ScopeGate),
        front_ends,
    )
    .await
    .unwrap_or_else(|failure| panic!("{failure}"));

    assert_eq!(report.front_ends, 7);
    assert_eq!(report.calls, fixture.cases.len() * 7);
}
