/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The runtime owns one live controller per resource: its text bindings stay
 * stable, its draft follows them, and staged branches stay private until
 * confirmed.
 */
import assert from "node:assert/strict";
import test from "node:test";

import { AuthoringRuntime } from "@clerkenwell/client";

import { createScratchTextBinding, insert, replaceAll } from "./support/text";

const resource = { entity: "Note", resourceKey: "note" } as const;

test("one runtime-owned authoring session keeps stable text bindings and materialises their draft", async () => {
  const runtime = new AuthoringRuntime();
  const initial = { prose: "A quiet beginning" };
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental"],
    baseline: initial,
    draft: initial
  });

  const binding = createScratchTextBinding(initial.prose);
  let disposed = false;
  const session = runtime.ensureController<typeof initial, "prose">(
    resource,
    () => ({
      currentDraft: () => ({ prose: binding.read() }),
      replaceDraft: (draft) => {
        if (binding.read() !== draft.prose) replaceAll(binding, draft.prose, "runtime-document");
      },
      bindText: () => binding,
      dispose: () => { disposed = true; }
    })
  );
  assert.equal(runtime.authoringSession(resource), session);
  assert.equal(session.bindText("prose"), session.bindText("prose"));

  insert(binding, initial.prose.length, " becomes unsettled");
  await new Promise<void>((resolve) => queueMicrotask(resolve));
  assert.equal(runtime.session<typeof initial>(resource)?.draft.prose, binding.read());

  const replacement = { prose: "A different draft" };
  runtime.modify(resource, replacement);
  assert.equal(binding.read(), replacement.prose);

  const renamed = { entity: "Note", resourceKey: "renamed-note" } as const;
  runtime.rename(resource, renamed);
  assert.equal(runtime.authoringSession(renamed), session);
  assert.deepEqual(session.resource, renamed);

  runtime.remove(renamed);
  assert.equal(disposed, true);
  assert.equal(runtime.authoringSession(renamed), undefined);
  assert.throws(() => session.bindText("prose"), /authoring session.*closed/i);
});

test("session-owned staged text stays private until confirmation and cancel discards only its branch", () => {
  const runtime = new AuthoringRuntime();
  const initial = { prose: "Accepted prose" };
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental"],
    baseline: initial,
    draft: initial
  });
  let controllerDraft = initial;
  const session = runtime.ensureController<typeof initial, "prose">(resource, () => ({
    currentDraft: () => controllerDraft,
    replaceDraft: (draft) => { controllerDraft = draft; },
    stageText: () => {
      const binding = createScratchTextBinding(controllerDraft.prose);
      return {
        binding,
        confirm: () => {
          controllerDraft = { prose: binding.read() };
          return { kind: "committed" };
        },
        dispose: () => undefined
      };
    }
  }));
  const stage = session.stageText("prose");
  const addition = " with private edits";
  insert(stage.binding, initial.prose.length, addition);

  assert.deepEqual(runtime.session(resource)?.draft, initial);
  assert.deepEqual(stage.confirm(), { kind: "committed" });
  assert.deepEqual(runtime.session(resource)?.draft, {
    prose: initial.prose + addition
  });

  const cancelled = session.stageText("prose");
  insert(cancelled.binding, 0, "Discarded ");
  cancelled.cancel();
  assert.deepEqual(runtime.session(resource)?.draft, {
    prose: initial.prose + addition
  });
});
