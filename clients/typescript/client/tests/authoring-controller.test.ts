/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The runtime owns one live controller per resource: its text bindings stay
 * stable, its draft follows them, and staged branches stay private until
 * confirmed.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  AuthoringRuntime,
  fromPersistedRuntime,
  toPersistedRuntime
} from "@clerkenwell/client";

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

function openedNote(
  runtime: AuthoringRuntime,
  target: { entity: string; resourceKey: string },
  policy: "collaborative" | "optimisticDocument"
): void {
  const initial = { prose: "Accepted prose" };
  runtime.open({
    resource: target,
    policy,
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: policy === "collaborative" ? ["incremental"] : ["optimisticDocument"],
    baseline: initial,
    draft: initial
  });
}

function persistedElsewhere(
  target: { entity: string; resourceKey: string },
  policy: "collaborative" | "optimisticDocument",
  draft: { prose: string }
) {
  const elsewhere = new AuthoringRuntime();
  openedNote(elsewhere, target, policy);
  elsewhere.modify(target, draft);
  return fromPersistedRuntime(toPersistedRuntime(elsewhere.getSnapshot()));
}

test("restoring takes another runtime's collaborative draft through the operations it carries", () => {
  const runtime = new AuthoringRuntime();
  openedNote(runtime, resource, "collaborative");
  const imported: unknown[] = [];
  const replaced: unknown[] = [];
  runtime.ensureController<{ prose: string }, "prose">(resource, () => ({
    replaceDraft: (draft) => { replaced.push(draft); },
    importDraft: (draft) => { imported.push(draft); }
  }));
  replaced.length = 0;
  const draft = { prose: "Carried with its operations" };

  runtime.restore(persistedElsewhere(resource, "collaborative", draft));

  assert.deepEqual(imported, [draft]);
  assert.deepEqual(replaced, []);
  assert.deepEqual(runtime.session(resource)?.draft, draft);
});

test("restoring replaces an optimistic session's draft and leaves unnamed sessions alone", () => {
  const runtime = new AuthoringRuntime();
  const other = { entity: "Note", resourceKey: "other-note" } as const;
  openedNote(runtime, resource, "optimisticDocument");
  openedNote(runtime, other, "collaborative");
  const replaced = new Map<string, unknown[]>([[resource.resourceKey, []], [other.resourceKey, []]]);
  for (const target of [resource, other]) {
    runtime.ensureController<{ prose: string }, "prose">(target, () => ({
      replaceDraft: (draft) => { replaced.get(target.resourceKey)?.push(draft); }
    }));
    replaced.get(target.resourceKey)!.length = 0;
  }
  const draft = { prose: "Edited in another window" };

  runtime.restore(persistedElsewhere(resource, "optimisticDocument", draft));

  assert.deepEqual(replaced.get(resource.resourceKey), [draft]);
  assert.deepEqual(replaced.get(other.resourceKey), []);
  assert.deepEqual(runtime.session(resource)?.draft, draft);
});
