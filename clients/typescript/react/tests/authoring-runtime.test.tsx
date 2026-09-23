/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The provider hands one application-owned runtime to the tree below it.
 */
import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { AuthoringRuntime, authoringSessionId } from "@clerkenwell/client";
import {
  AuthoringRuntimeProvider,
  useAuthoringRuntime,
  useAuthoringRuntimeSnapshot
} from "@clerkenwell/react";

const resource = { entity: "Memo", resourceKey: "memo-1" } as const;

function openedRuntime(): AuthoringRuntime {
  const runtime = new AuthoringRuntime();
  runtime.open({
    resource,
    policy: "collaborative",
    schemaVersion: 1,
    acceptedRevision: "accepted",
    supportedExchangeModes: ["incremental"],
    baseline: { text: "server copy" },
    draft: { text: "server copy" }
  });
  return runtime;
}

function SessionStatus(): React.ReactElement {
  const snapshot = useAuthoringRuntimeSnapshot();
  return <p>{snapshot.sessions[authoringSessionId(resource)]?.status}</p>;
}

test("components below the provider read the runtime it was given", () => {
  const runtime = openedRuntime();
  let seen: AuthoringRuntime | null = null;
  function Probe(): null {
    seen = useAuthoringRuntime();
    return null;
  }

  renderToStaticMarkup(
    <AuthoringRuntimeProvider runtime={runtime}>
      <Probe />
    </AuthoringRuntimeProvider>
  );

  assert.equal(seen, runtime);
});

test("the snapshot hook reads the runtime's current sessions", () => {
  const runtime = openedRuntime();
  const render = () => renderToStaticMarkup(
    <AuthoringRuntimeProvider runtime={runtime}>
      <SessionStatus />
    </AuthoringRuntimeProvider>
  );

  assert.equal(render(), `<p>${runtime.session(resource)?.status}</p>`);
  runtime.modify(resource, { text: "local edit" });
  assert.equal(render(), `<p>${runtime.session(resource)?.status}</p>`);
});

test("reading the runtime outside a provider is refused", () => {
  function Orphan(): null {
    useAuthoringRuntime();
    return null;
  }

  assert.throws(() => renderToStaticMarkup(<Orphan />), /AuthoringRuntimeProvider/);
});
