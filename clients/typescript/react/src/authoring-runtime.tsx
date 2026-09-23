/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * React binding for the React-free authoring runtime. The application
 * constructs the runtime and chooses where its persisted snapshot lives.
 */
import React from "react";

import {
  durableSessions,
  sameSessions,
  toPersistedRuntime,
  type AuthoringRuntime,
  type AuthoringRuntimeState,
  type PersistedAuthoringRuntimeState
} from "@clerkenwell/client";

const AuthoringRuntimeContext = React.createContext<AuthoringRuntime | null>(null);

export function AuthoringRuntimeProvider({
  runtime,
  children
}: {
  runtime: AuthoringRuntime;
  children: React.ReactNode;
}): React.JSX.Element {
  return (
    <AuthoringRuntimeContext.Provider value={runtime}>
      {children}
    </AuthoringRuntimeContext.Provider>
  );
}

export function useAuthoringRuntime(): AuthoringRuntime {
  const runtime = React.useContext(AuthoringRuntimeContext);
  if (runtime === null) {
    throw new Error("useAuthoringRuntime must be used inside AuthoringRuntimeProvider.");
  }
  return runtime;
}

export function useAuthoringRuntimeSnapshot(): AuthoringRuntimeState {
  const runtime = useAuthoringRuntime();
  return React.useSyncExternalStore(
    runtime.subscribe,
    runtime.getSnapshot,
    runtime.getSnapshot
  );
}

/**
 * Persist the runtime's snapshot whenever its durable sessions change. A
 * publish for a clean session, or for one already written, writes nothing.
 */
export function useAuthoringRuntimePersistence(
  runtime: AuthoringRuntime,
  persist: (state: PersistedAuthoringRuntimeState) => void
): void {
  const persistedSessionsRef = React.useRef(durableSessions(runtime.getSnapshot()));
  React.useEffect(
    () => runtime.subscribe(() => {
      const snapshot = runtime.getSnapshot();
      const durable = durableSessions(snapshot);
      if (sameSessions(durable, persistedSessionsRef.current)) {
        return;
      }
      persistedSessionsRef.current = durable;
      persist(toPersistedRuntime(snapshot));
    }),
    [runtime, persist]
  );
}
