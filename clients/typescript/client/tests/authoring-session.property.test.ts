/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * An acknowledgement settles the commit it is about and nothing later.
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  acknowledgeAuthoringOperation,
  beginAuthoringReplay,
  modifyAuthoringSession,
  queueAuthoringOperation,
  startAuthoringSession
} from "@clerkenwell/client";

test("commit acknowledgements preserve every causally later local draft", () => {
  fc.assert(
    fc.property(
      // A commit has to change something. Where the committed text equals the
      // baseline the draft never leaves `clean`, so there is no local work to
      // queue and no commit for an acknowledgement to be about — the state
      // machine refuses that queue, and rightly. The property is about what an
      // acknowledgement does to a draft made after the commit, so it draws a
      // commit that is one.
      fc
        .tuple(fc.string(), fc.string())
        .filter(([baselineText, committedText]) => baselineText !== committedText),
      fc.string(),
      ([baselineText, committedText], laterText) => {
        const resource = { entity: "Note", resourceKey: "note" };
        const clean = startAuthoringSession({
          resource,
          policy: "collaborative",
          schemaVersion: 1,
          acceptedRevision: "base",
          supportedExchangeModes: ["incremental", "bootstrap"],
          baseline: { text: baselineText },
          draft: { text: baselineText }
        });
        const modified = modifyAuthoringSession(clean, {
          text: committedText
        });
        const queued = queueAuthoringOperation(modified, {
          operationId: "operation",
          schemaVersion: 1,
          exchangeMode: "incremental",
          baseRevision: "base",
          updateBase64: "update"
        });
        const sending = beginAuthoringReplay(queued, "operation");
        const finalBeforeAcknowledgement = modifyAuthoringSession(
          sending,
          { text: laterText }
        );
        const acknowledged = acknowledgeAuthoringOperation({
          session: finalBeforeAcknowledgement,
          operationId: "operation",
          document: { text: committedText },
          acceptedRevision: "accepted"
        });

        assert.deepEqual(acknowledged.baseline, { text: committedText });
        assert.deepEqual(
          acknowledged.draft,
          committedText === laterText
            ? { text: committedText }
            : { text: laterText }
        );
        assert.equal(
          acknowledged.status,
          committedText === laterText ? "clean" : "modified"
        );
      }
    ),
    { numRuns: 200 }
  );
});
