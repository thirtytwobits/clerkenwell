/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Whatever order saves, remote edits and echoes arrive in, a session's draft
 * converges on the accepted document.
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { AuthoringRuntime } from "@clerkenwell/client";

import {
  FakeBoardServer,
  openBoardSession,
  relabelTask,
  renameTask
} from "./support/board-server";
import { boardDocument } from "./support/plans";

type TaskId = "task-1" | "task-2" | "task-3";

type Step =
  | { kind: "save"; task: TaskId; labels: string[] }
  | { kind: "remote"; task: TaskId; title: string }
  | { kind: "echo" };

const taskArbitrary = fc.constantFrom<TaskId>("task-1", "task-2", "task-3");
const stepArbitrary: fc.Arbitrary<Step> = fc.oneof(
  fc.record({
    kind: fc.constant("save" as const),
    task: taskArbitrary,
    labels: fc.array(fc.constantFrom("red", "amber", "green"), { maxLength: 3 })
  }),
  fc.record({
    kind: fc.constant("remote" as const),
    task: taskArbitrary,
    title: fc.string({ minLength: 1, maxLength: 12 })
  }),
  fc.constant({ kind: "echo" as const })
);

test("a session converges under arbitrary saves, remote edits and echoes", () => {
  fc.assert(
    fc.property(fc.array(stepArbitrary, { minLength: 1, maxLength: 12 }), (steps) => {
      const server = new FakeBoardServer(boardDocument());
      const runtime = new AuthoringRuntime();
      const session = openBoardSession(runtime, server, { entity: "Board", resourceKey: "board-1" });

      for (const step of steps) {
        let accepted = server.snapshot();
        if (step.kind === "save") {
          const base = session.state().acceptedRevision;
          session.replaceDraft(relabelTask(session.currentDraft(), step.task, step.labels));
          accepted = server.accept({
            baseFrontierBase64: base,
            updateBase64: session.exportIncrementalUpdateBase64(base)
          }).state;
        } else if (step.kind === "remote") {
          accepted = server.editRemotely((board) => renameTask(board, step.task, step.title));
        }
        session.adoptAccepted({
          updateBase64: accepted.update_base64,
          baseline: server.board(),
          acceptedFrontierBase64: accepted.accepted_frontier_base64,
          acceptedRevision: accepted.accepted_frontier_base64
        });
        assert.deepEqual(session.currentDraft(), server.board());
      }
    }),
    { numRuns: 30 }
  );
});
