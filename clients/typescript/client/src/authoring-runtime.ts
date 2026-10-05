/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The authoring runtime: the one owner of an application's authoring sessions
 * and their offline operations, and the controllers it hands out.
 */

import type { AuthoringResourceIdentity, AuthoringRuntimeListener, AuthoringRuntimeState, AuthoringSession, AuthoringSessionController, AuthoringTextStageConfirmation, AuthoringTextStageController, QueuedAuthoringOperation, RedactedAuthoringRuntimeDiagnostic, RedactedAuthoringSessionDiagnostic } from "./authoring-session.js";
import { acknowledgeAuthoringOperation, adoptAuthoringBaseline, authoringSessionAcceptsDraft, authoringSessionId, authoringSessionRequiresDurableRestoration, beginAuthoringReplay, blockAuthoringSession, bootstrapAuthoringSession, discardAuthoringChanges, disconnectAuthoringSession, documentsEqual, modifyAuthoringSession, queueAuthoringOperation, reconnectAuthoringSession, rejectAuthoringOperation, startAuthoringSession, supersedeBlockedAuthoringOperations } from "./authoring-state-machine.js";
import type { TextBinding } from "./text-binding.js";

/**
 * A controller whose replica records a session's draft as operations and
 * takes them back: {@link AuthoringSession.draftOperations}.
 */
type OperationRecordingController = AuthoringSessionController<unknown, string>
  & Required<Pick<
    AuthoringSessionController<unknown, string>,
    | "acceptedFrontierBase64"
    | "coversFrontierBase64"
    | "exportIncrementalUpdateBase64"
    | "importUpdateBase64"
  >>;

function recordsOperations(
  controller: AuthoringSessionController<unknown, string>
): controller is OperationRecordingController {
  return controller.acceptedFrontierBase64 !== undefined
    && controller.coversFrontierBase64 !== undefined
    && controller.exportIncrementalUpdateBase64 !== undefined
    && controller.importUpdateBase64 !== undefined;
}

interface OwnedAuthoringTextStage {
  closed: boolean;
  controller: AuthoringTextStageController;
}

interface OwnedAuthoringController {
  resource: AuthoringResourceIdentity;
  controller: AuthoringSessionController<unknown, string>;
  handle?: AuthoringSessionHandle<unknown, string>;
  bindingSubscriptions: Map<TextBinding, () => void>;
  textStages: Set<OwnedAuthoringTextStage>;
  draftSyncQueued: boolean;
  disposed: boolean;
  /**
   * Frontier of the newest accepted state the replica holds, which a session's
   * recorded operations extend: where the replica stood when attached, then
   * each accepted frontier it takes through the session handle.
   */
  acceptedBaseFrontierBase64?: string;
  /**
   * Whether the session holds pending work the replica could not take. Its
   * draft and recorded operations, or their absence, stay as they are until
   * the session has nothing pending.
   */
  holdsUntakenDraft: boolean;
}

/**
 * Stable capability for one private Save/Cancel text branch.
 *
 * Edits stay out of the resource draft until confirmation. A rejected
 * confirmation leaves the branch and its exact editor history available.
 */
export class AuthoringTextStageHandle {
  constructor(
    private readonly runtime: AuthoringRuntime,
    private readonly owner: OwnedAuthoringController,
    private readonly owned: OwnedAuthoringTextStage
  ) {}

  get binding(): TextBinding {
    return this.owned.controller.binding;
  }

  get closed(): boolean {
    return this.owned.closed;
  }

  confirm(): AuthoringTextStageConfirmation {
    this.requireOpen();
    if (this.owner.disposed) {
      return {
        kind: "blocked",
        message: "The authored resource is no longer open.",
        reason: "schemaChanged"
      };
    }
    const result = this.owned.controller.confirm();
    if (result.kind === "committed") {
      this.runtime.syncControllerDraft(this.owner);
      this.close();
    }
    return result;
  }

  cancel(): void {
    this.requireOpen();
    this.close();
  }

  private close(): void {
    this.owned.controller.dispose();
    this.owned.closed = true;
    this.owner.textStages.delete(this.owned);
  }

  private requireOpen(): void {
    if (this.owned.closed) {
      throw new Error("This staged text branch is already closed.");
    }
  }
}

/** Stable live capability for one resource already opened in AuthoringRuntime. */
export class AuthoringSessionHandle<
  TDocument,
  TTextFieldPath extends string = string
> {
  constructor(
    private readonly runtime: AuthoringRuntime,
    private readonly owned: OwnedAuthoringController
  ) {}

  get resource(): AuthoringResourceIdentity {
    return { ...this.owned.resource };
  }

  state(): AuthoringSession<TDocument> {
    const state = this.runtime.session<TDocument>(this.owned.resource);
    if (state === undefined) {
      throw new Error(
        `No authoring session exists for ${JSON.stringify(authoringSessionId(this.owned.resource))}.`
      );
    }
    return state;
  }

  currentDraft(): TDocument {
    const controller = this.controller();
    return controller.currentDraft === undefined
      ? structuredClone(this.state().draft)
      : structuredClone(controller.currentDraft());
  }

  replaceDraft(draft: TDocument, validation?: unknown): void {
    this.runtime.modify(this.owned.resource, draft, validation);
  }

  /**
   * Import accepted operations before reconciling the retained local draft.
   * `updateBase64` holds what the replica lacks of the state accepted at
   * `acceptedFrontierBase64`, so a replica already holding that frontier takes
   * nothing from it. The baseline defaults to the draft the replica holds at
   * that frontier once it has taken the update. A session that does not
   * accept a draft takes none of it: its replica, draft and baseline stay as
   * they are until its block is resolved.
   */
  adoptAccepted(input: {
    updateBase64: string;
    acceptedFrontierBase64: string;
    schemaVersion?: number;
    baseline?: TDocument;
    draft?: TDocument;
    acceptedRevision: string;
    validation?: unknown;
  }): void {
    const controller = this.controller();
    if (!authoringSessionAcceptsDraft(this.state())) {
      return;
    }
    if (input.schemaVersion === undefined) {
      if (controller.importUpdateBase64 === undefined) {
        throw new Error(`${this.owned.resource.entity} authoring cannot import Loro updates.`);
      }
      if (!holdsFrontier(controller, input.acceptedFrontierBase64)) {
        controller.importUpdateBase64(input.updateBase64);
      }
    } else {
      if (controller.importVersionedUpdateBase64 === undefined) {
        throw new Error(
          `${this.owned.resource.entity} authoring cannot import versioned Loro updates.`
        );
      }
      controller.importVersionedUpdateBase64(
        input.schemaVersion,
        input.updateBase64
      );
    }
    takeAcceptedFrontier(this.owned, input.acceptedFrontierBase64);
    const baseline = () => input.baseline ?? this.draftAt(input.acceptedFrontierBase64);
    const draft = input.draft
      ?? controller.currentDraft?.()
      ?? baseline();
    if (this.state().queuedOperations.length > 0) {
      this.runtime.modify(this.owned.resource, draft);
      return;
    }
    this.runtime.adoptBaseline(this.owned.resource, {
      baseline: baseline(),
      draft,
      acceptedRevision: input.acceptedRevision,
      validation: input.validation
    });
  }

  bindText(
    fieldPath: TTextFieldPath,
    identities: Readonly<Record<string, string>> = {}
  ): TextBinding {
    const controller = this.controller();
    if (controller.bindText === undefined) {
      throw new Error(
        `${this.owned.resource.entity} authoring does not expose collaborative text fields.`
      );
    }
    const binding = controller.bindText(fieldPath, identities);
    if (!this.owned.bindingSubscriptions.has(binding)) {
      this.owned.bindingSubscriptions.set(
        binding,
        binding.subscribe(() => this.runtime.scheduleControllerDraftSync(this.owned))
      );
    }
    return binding;
  }

  stageText(
    fieldPath: TTextFieldPath,
    identities: Readonly<Record<string, string>> = {}
  ): AuthoringTextStageHandle {
    const controller = this.controller();
    if (controller.stageText === undefined) {
      throw new Error(
        `${this.owned.resource.entity} authoring does not expose staged text fields.`
      );
    }
    const owned: OwnedAuthoringTextStage = {
      closed: false,
      controller: controller.stageText(fieldPath, identities)
    };
    this.owned.textStages.add(owned);
    return new AuthoringTextStageHandle(this.runtime, this.owned, owned);
  }

  adoptDocument(document: TDocument): void {
    const controller = this.controller();
    if (controller.adoptDocument === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot adopt a document.`);
    }
    controller.adoptDocument(document);
  }

  /**
   * Import what the replica lacks of the state accepted at
   * `acceptedFrontierBase64`, and materialise the draft it leaves. A replica
   * already holding that frontier takes nothing from the update. A session
   * that does not accept a draft takes none of it.
   */
  importUpdateBase64(updateBase64: string, acceptedFrontierBase64: string): void {
    const controller = this.controller();
    if (controller.importUpdateBase64 === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot import Loro updates.`);
    }
    if (!authoringSessionAcceptsDraft(this.state())) {
      return;
    }
    if (!holdsFrontier(controller, acceptedFrontierBase64)) {
      controller.importUpdateBase64(updateBase64);
    }
    takeAcceptedFrontier(this.owned, acceptedFrontierBase64);
    this.runtime.syncControllerDraft(this.owned);
  }

  /**
   * Import what the replica lacks of the state accepted at
   * `acceptedFrontierBase64`, and materialise the draft it leaves. A session
   * that does not accept a draft takes none of it.
   */
  importVersionedUpdateBase64(
    schemaVersion: number,
    updateBase64: string,
    acceptedFrontierBase64: string
  ): void {
    const controller = this.controller();
    if (controller.importVersionedUpdateBase64 === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot import versioned Loro updates.`);
    }
    if (!authoringSessionAcceptsDraft(this.state())) {
      return;
    }
    controller.importVersionedUpdateBase64(schemaVersion, updateBase64);
    takeAcceptedFrontier(this.owned, acceptedFrontierBase64);
    this.runtime.syncControllerDraft(this.owned);
  }

  /** The draft as it stood at a frontier the live replica holds, such as an accepted one. */
  draftAt(frontierBase64: string): TDocument {
    const operation = this.controller().draftAt;
    if (operation === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot read a draft at a frontier.`);
    }
    return operation(frontierBase64);
  }

  exportUpdateBase64(): string {
    const operation = this.controller().exportUpdateBase64;
    if (operation === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot export Loro updates.`);
    }
    return operation();
  }

  exportIncrementalUpdateBase64(acceptedFrontierBase64: string): string {
    const operation = this.controller().exportIncrementalUpdateBase64;
    if (operation === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring cannot export incremental Loro updates.`);
    }
    return operation(acceptedFrontierBase64);
  }

  acceptedFrontierBase64(): string {
    const operation = this.controller().acceptedFrontierBase64;
    if (operation === undefined) {
      throw new Error(`${this.owned.resource.entity} authoring has no Loro frontier.`);
    }
    return operation();
  }

  private controller(): AuthoringSessionController<TDocument, TTextFieldPath> {
    if (this.owned.disposed) {
      throw new Error(
        `The authoring session for ${JSON.stringify(authoringSessionId(this.owned.resource))} is closed.`
      );
    }
    return this.owned.controller as AuthoringSessionController<TDocument, TTextFieldPath>;
  }
}

export function redactedAuthoringRuntimeDiagnostic(
  runtime: AuthoringRuntime | AuthoringRuntimeState
): RedactedAuthoringRuntimeDiagnostic {
  const state = runtime instanceof AuthoringRuntime
    ? runtime.getSnapshot()
    : runtime;
  const sessions = Object.values(state.sessions)
    .map((session): RedactedAuthoringSessionDiagnostic => {
      const rejectionCategories = new Set<string>();
      for (const operation of session.queuedOperations) {
        if (operation.rejectionCategory !== undefined) {
          rejectionCategories.add(operation.rejectionCategory);
        }
      }
      if (session.lastRejection !== undefined) {
        rejectionCategories.add(session.lastRejection.category);
      }
      return {
        resource: { ...session.resource },
        policy: session.policy,
        status: session.status,
        schemaVersion: session.schemaVersion,
        queuedOperationCount: session.queuedOperations.length,
        sendingOperationCount: session.queuedOperations.filter(
          ({ state }) => state === "sending"
        ).length,
        blockedOperationCount: session.queuedOperations.filter(
          ({ state: operationState }) => operationState === "blocked"
        ).length,
        retryCount: session.queuedOperations.reduce(
          (total, operation) => total + operation.retryCount,
          0
        ),
        rejectionCategories: [...rejectionCategories].sort()
      };
    })
    .sort((left, right) =>
      authoringSessionId(left.resource).localeCompare(
        authoringSessionId(right.resource)
      )
    );
  return {
    sessionCount: sessions.length,
    queuedOperationCount: sessions.reduce(
      (total, session) => total + session.queuedOperationCount,
      0
    ),
    blockedOperationCount: sessions.reduce(
      (total, session) => total + session.blockedOperationCount,
      0
    ),
    sessions,
    redaction: "metadataOnly"
  };
}

/**
 * The single React-free owner of authoring sessions and offline operations.
 *
 * UI bindings may subscribe to this store and the application may persist
 * its snapshot, but neither owns a second draft lifecycle.
 */
export class AuthoringRuntime {
  readonly #listeners = new Set<AuthoringRuntimeListener>();
  readonly #controllers = new Map<string, OwnedAuthoringController>();
  #state: AuthoringRuntimeState;
  #carried: Readonly<Record<string, AuthoringSession<unknown>>> = {};
  #batchDepth = 0;
  #batchDirty = false;

  /**
   * Start from a persisted snapshot, as after a restart. An operation that was
   * being sent when the snapshot was taken is queued again.
   */
  constructor(initial: AuthoringRuntimeState = { sessions: {} }) {
    this.#state = cloneRuntimeState(initial, true);
  }

  getSnapshot = (): AuthoringRuntimeState => this.#state;

  subscribe = (listener: AuthoringRuntimeListener): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /**
   * Apply several transitions and notify subscribers once, after the last one.
   *
   * Every subscriber sees every publish, so a loop that opens or adopts one
   * session per record would otherwise re-render each binding and rewrite the
   * persisted snapshot once per record. Batches nest; the outermost one
   * notifies.
   */
  batch(work: () => void): void {
    this.#batchDepth += 1;
    try {
      work();
    } finally {
      this.#batchDepth -= 1;
    }
    if (this.#batchDepth === 0 && this.#batchDirty) {
      this.#batchDirty = false;
      this.#notify();
    }
  }

  /**
   * Adopt the sessions another live runtime persisted on top of this one's.
   *
   * A snapshot carries only the sessions that
   * {@link authoringSessionRequiresDurableRestoration} keeps, so a session it
   * omits means "nothing pending for this resource", not "this resource is
   * gone". Restoring therefore overwrites what the snapshot names and leaves
   * everything else where it is.
   *
   * A collaborative draft materialises operations the other runtime holds and
   * sends itself, so this runtime never writes it into a replica. A
   * collaborative session with a live controller is restored only through
   * {@link AuthoringSessionController.importDraft}; without it the live session
   * stays, and the edits arrive as accepted state. A collaborative session this
   * runtime has no controller for is carried rather than adopted: it is
   * persisted with this runtime's own sessions ({@link persistedState}) until a
   * later snapshot omits it, and never attached here.
   */
  restore(snapshot: AuthoringRuntimeState): void {
    const restored = cloneRuntimeState(snapshot);
    const sessions = { ...this.#state.sessions };
    const carried: Record<string, AuthoringSession<unknown>> = {};
    let changed = false;
    for (const [id, session] of Object.entries(restored.sessions)) {
      const live = this.#state.sessions[id];
      const controller = this.#controllers.get(id)?.controller;
      if (session.policy === "collaborative" && controller === undefined) {
        carried[id] = session;
        continue;
      }
      if (documentsEqual(session, live)) {
        continue;
      }
      if (live?.policy === "collaborative" && controller !== undefined) {
        if (controller.importDraft === undefined) {
          continue;
        }
        controller.importDraft(session.draft);
      } else {
        controller?.replaceDraft?.(session.draft);
      }
      sessions[id] = session;
      changed = true;
    }
    this.#carried = carried;
    if (changed) {
      this.#publish({ sessions });
    }
  }

  /**
   * The sessions to persist: this runtime's own, and each session carried from
   * another runtime where this runtime has nothing pending of its own.
   */
  persistedState(): AuthoringRuntimeState {
    const sessions = { ...this.#state.sessions };
    for (const [id, session] of Object.entries(this.#carried)) {
      const own = sessions[id];
      if (own === undefined || !authoringSessionRequiresDurableRestoration(own)) {
        sessions[id] = session;
      }
    }
    return { sessions };
  }

  session<TDocument>(
    resource: AuthoringResourceIdentity
  ): AuthoringSession<TDocument> | undefined {
    return this.#state.sessions[authoringSessionId(resource)] as
      | AuthoringSession<TDocument>
      | undefined;
  }

  /**
   * Install the one live controller for an opened resource, or return its
   * existing stable session handle. The controller itself never enters the
   * serialisable runtime snapshot.
   */
  ensureController<TDocument, TTextFieldPath extends string>(
    resource: AuthoringResourceIdentity,
    create: () => AuthoringSessionController<TDocument, TTextFieldPath>
  ): AuthoringSessionHandle<TDocument, TTextFieldPath> {
    const id = authoringSessionId(resource);
    const session = this.session<TDocument>(resource);
    if (session === undefined) {
      throw new Error(`Cannot attach live authoring before opening ${JSON.stringify(id)}.`);
    }
    const existing = this.#controllers.get(id);
    if (existing !== undefined) {
      return existing.handle as AuthoringSessionHandle<TDocument, TTextFieldPath>;
    }
    const controller = create() as AuthoringSessionController<unknown, string>;
    const recording = recordsOperations(controller);
    const owned: OwnedAuthoringController = {
      resource: { ...resource },
      controller,
      bindingSubscriptions: new Map(),
      textStages: new Set(),
      draftSyncQueued: false,
      disposed: false,
      holdsUntakenDraft: false,
      ...(recording ? { acceptedBaseFrontierBase64: controller.acceptedFrontierBase64() } : {})
    };
    // Recorded operations are taken as themselves, so edits the replica
    // already holds are not authored again. A replica without their base holds
    // other history, and a blocked session's draft stays as it is: both take
    // the draft as a document. A replica that takes no documents holds a
    // blocked draft only as its operations, and cannot hold pending work
    // without them: that session is held for recovery with its draft and
    // operations as they are.
    const operations = session.draftOperations;
    if (
      recording
      && operations !== undefined
      && (authoringSessionAcceptsDraft(session) || controller.replaceDraft === undefined)
      && controller.coversFrontierBase64(operations.baseFrontierBase64)
    ) {
      controller.importUpdateBase64(operations.updateBase64);
    } else if (controller.replaceDraft !== undefined) {
      controller.replaceDraft(session.draft);
    } else if (
      (recording || controller.currentDraft !== undefined)
      && authoringSessionRequiresDurableRestoration(session)
    ) {
      owned.holdsUntakenDraft = true;
    }
    owned.handle = new AuthoringSessionHandle<unknown, string>(this, owned);
    this.#controllers.set(id, owned);
    if (owned.holdsUntakenDraft && authoringSessionAcceptsDraft(session)) {
      this.#setSession(blockAuthoringSession(session, "recoveryRequired"), false);
    }
    this.syncControllerDraft(owned);
    const synced = this.session<unknown>(resource);
    if (synced !== undefined) {
      const recorded = recordDraftOperations(owned, synced);
      if (recorded !== synced) {
        this.#publish({ sessions: { ...this.#state.sessions, [id]: recorded } });
      }
    }
    return owned.handle as AuthoringSessionHandle<TDocument, TTextFieldPath>;
  }

  authoringSession<TDocument, TTextFieldPath extends string = string>(
    resource: AuthoringResourceIdentity
  ): AuthoringSessionHandle<TDocument, TTextFieldPath> | undefined {
    const owned = this.#controllers.get(authoringSessionId(resource));
    return owned === undefined
      ? undefined
      : owned.handle as AuthoringSessionHandle<TDocument, TTextFieldPath>;
  }

  /** Domain adapters use their runtime-owned controller for non-text commands. */
  controller<TController>(
    resource: AuthoringResourceIdentity
  ): TController | undefined {
    return this.#controllers.get(authoringSessionId(resource))?.controller as
      | TController
      | undefined;
  }

  replaceController<TDocument, TTextFieldPath extends string>(
    resource: AuthoringResourceIdentity,
    controller: AuthoringSessionController<TDocument, TTextFieldPath>
  ): AuthoringSessionHandle<TDocument, TTextFieldPath> {
    this.detachController(resource);
    return this.ensureController(resource, () => controller);
  }

  detachController(resource: AuthoringResourceIdentity): void {
    const id = authoringSessionId(resource);
    const owned = this.#controllers.get(id);
    if (owned === undefined) {
      return;
    }
    this.#controllers.delete(id);
    this.#disposeController(owned);
  }

  open<TDocument>(input: Parameters<typeof startAuthoringSession<TDocument>>[0]): void {
    this.#setSession(startAuthoringSession(input));
  }

  bootstrap<TDocument>(
    input: Parameters<typeof bootstrapAuthoringSession<TDocument>>[0]
  ): void {
    this.#setSession(bootstrapAuthoringSession(input));
  }

  modify<TDocument>(
    resource: AuthoringResourceIdentity,
    draft: TDocument,
    validation?: unknown
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    const owned = this.#controllers.get(authoringSessionId(resource));
    const controller = owned?.controller as
      | AuthoringSessionController<TDocument, string>
      | undefined;
    controller?.replaceDraft?.(draft);
    const materializedDraft = controller?.currentDraft?.() ?? draft;
    this.#setSession(
      modifyAuthoringSession(session, materializedDraft, validation),
      false
    );
  }

  adoptBaseline<TDocument>(
    resource: AuthoringResourceIdentity,
    input: Omit<Parameters<typeof adoptAuthoringBaseline<TDocument>>[0], "session">
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    const adopted = adoptAuthoringBaseline({ session, ...input });
    if (adopted !== session) {
      this.#setSession(adopted);
    }
  }

  queue<TDocument>(
    resource: AuthoringResourceIdentity,
    operation: Omit<QueuedAuthoringOperation<TDocument>, "resource" | "retryCount" | "state">
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(queueAuthoringOperation(session, operation));
  }

  beginReplay<TDocument>(
    resource: AuthoringResourceIdentity,
    operationId: string
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(beginAuthoringReplay(session, operationId));
  }

  acknowledge<TDocument>(input: {
    resource: AuthoringResourceIdentity;
    operationId: string;
    document: TDocument;
    acceptedRevision: string;
  }): void {
    const session = this.#requireSession<TDocument>(input.resource);
    this.#setSession(acknowledgeAuthoringOperation({ session, ...input }));
  }

  /**
   * Acknowledge an operation only while it is still owned by this runtime.
   *
   * Projection reconciliation can consume an accepted operation before the
   * originating async call settles. That completion race is not an unknown
   * operation error: the accepted state already owns the outcome.
   */
  acknowledgeIfQueued<TDocument>(input: {
    resource: AuthoringResourceIdentity;
    operationId: string;
    document: TDocument;
    acceptedRevision: string;
  }): boolean {
    const session = this.session<TDocument>(input.resource);
    if (
      session === undefined
      || !session.queuedOperations.some(
        ({ operationId }) => operationId === input.operationId
      )
    ) {
      return false;
    }
    this.#setSession(acknowledgeAuthoringOperation({ session, ...input }));
    return true;
  }

  reject<TDocument>(input: {
    resource: AuthoringResourceIdentity;
    operationId: string;
    category: string;
    message: string;
    retryable: boolean;
  }): void {
    const session = this.#requireSession<TDocument>(input.resource);
    this.#setSession(rejectAuthoringOperation({ session, ...input }));
  }

  /**
   * Reject an operation only while it is still owned by this runtime.
   *
   * Accepted projection reconciliation can consume an operation before the
   * originating async call settles. That completion race is not an unknown
   * operation error: the accepted state already owns the outcome.
   */
  rejectIfQueued<TDocument>(input: {
    resource: AuthoringResourceIdentity;
    operationId: string;
    category: string;
    message: string;
    retryable: boolean;
  }): boolean {
    const session = this.session<TDocument>(input.resource);
    if (
      session === undefined
      || !session.queuedOperations.some(
        ({ operationId }) => operationId === input.operationId
      )
    ) {
      return false;
    }
    this.#setSession(rejectAuthoringOperation({ session, ...input }));
    return true;
  }

  supersedeBlocked<TDocument>(
    resource: AuthoringResourceIdentity
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(supersedeBlockedAuthoringOperations(session));
  }

  disconnect(resource?: AuthoringResourceIdentity): void {
    this.#mapSelected(resource, (session) => disconnectAuthoringSession(session));
  }

  reconnect(resource?: AuthoringResourceIdentity): void {
    this.#mapSelected(resource, (session) => {
      if (
        session.status !== "disconnectedReadable"
        && session.status !== "offlineModified"
      ) {
        return session;
      }
      return reconnectAuthoringSession(session);
    });
  }

  block<TDocument>(
    resource: AuthoringResourceIdentity,
    status: Parameters<typeof blockAuthoringSession<TDocument>>[1]
  ): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(blockAuthoringSession(session, status));
  }

  discard<TDocument>(resource: AuthoringResourceIdentity): void {
    const session = this.#requireSession<TDocument>(resource);
    this.#setSession(discardAuthoringChanges(session));
  }

  remove(resource: AuthoringResourceIdentity): void {
    const id = authoringSessionId(resource);
    this.detachController(resource);
    if (!(id in this.#state.sessions)) {
      return;
    }
    const sessions = { ...this.#state.sessions };
    delete sessions[id];
    this.#publish({ sessions });
  }

  rename(from: AuthoringResourceIdentity, to: AuthoringResourceIdentity): void {
    const current = this.#requireSession(from);
    const fromId = authoringSessionId(from);
    const toId = authoringSessionId(to);
    const owned = this.#controllers.get(fromId);
    if (owned !== undefined) {
      const displaced = this.#controllers.get(toId);
      if (displaced !== undefined && displaced !== owned) {
        this.#disposeController(displaced);
      }
      this.#controllers.delete(fromId);
      owned.resource = { ...to };
      this.#controllers.set(toId, owned);
    }
    const sessions = { ...this.#state.sessions };
    delete sessions[fromId];
    const renamed = {
      ...current,
      resource: { ...to },
      queuedOperations: current.queuedOperations.map((operation) => ({
        ...operation,
        resource: { ...to }
      }))
    };
    sessions[toId] = renamed;
    this.#publish({ sessions });
  }

  /** Queue draft materialisation after a bound editor transaction finishes. */
  scheduleControllerDraftSync(owned: OwnedAuthoringController): void {
    if (owned.draftSyncQueued) {
      return;
    }
    owned.draftSyncQueued = true;
    queueMicrotask(() => {
      owned.draftSyncQueued = false;
      if (this.#controllers.get(authoringSessionId(owned.resource)) === owned) {
        this.syncControllerDraft(owned);
      }
    });
  }

  /**
   * Materialise the controller into the serialisable draft without writing
   * back. A blocked session's draft stays as it is.
   */
  syncControllerDraft(owned: OwnedAuthoringController): void {
    const controller = owned.controller;
    if (controller.currentDraft === undefined || owned.holdsUntakenDraft) {
      return;
    }
    const session = this.session<unknown>(owned.resource);
    if (session === undefined || !authoringSessionAcceptsDraft(session)) {
      return;
    }
    const draft = controller.currentDraft();
    if (documentsEqual(session.draft, draft)) {
      return;
    }
    this.#setSession(modifyAuthoringSession(session, draft), false);
  }

  dispose(): void {
    for (const owned of this.#controllers.values()) {
      this.#disposeController(owned);
    }
    this.#controllers.clear();
    this.#listeners.clear();
  }

  #requireSession<TDocument>(
    resource: AuthoringResourceIdentity
  ): AuthoringSession<TDocument> {
    const session = this.session<TDocument>(resource);
    if (session === undefined) {
      throw new Error(
        `No authoring session exists for ${JSON.stringify(authoringSessionId(resource))}.`
      );
    }
    return session;
  }

  // Transitions build fresh session objects and clone the documents they take
  // in, so the stored session needs no further copy. Keeping the transition's
  // object means a field a transition left alone (a baseline while the draft
  // changes, every other session while one changes) keeps its identity, and
  // bindings that memoise on those identities stay put.
  #setSession<TDocument>(
    session: AuthoringSession<TDocument>,
    alignController: boolean = true
  ): void {
    const id = authoringSessionId(session.resource);
    const owned = this.#controllers.get(id);
    if (owned !== undefined && !authoringSessionRequiresDurableRestoration(session)) {
      owned.holdsUntakenDraft = false;
    }
    if (alignController) {
      owned?.controller.replaceDraft?.(session.draft);
    }
    this.#publish({
      sessions: {
        ...this.#state.sessions,
        [id]: owned === undefined
          ? withoutStaleDraftOperations(this.#state.sessions[id], session)
          : recordDraftOperations(owned, session)
      }
    });
  }

  #mapSelected(
    resource: AuthoringResourceIdentity | undefined,
    transition: (session: AuthoringSession<unknown>) => AuthoringSession<unknown>
  ): void {
    const ids = resource === undefined
      ? Object.keys(this.#state.sessions)
      : [authoringSessionId(resource)];
    let changed = false;
    const sessions = { ...this.#state.sessions };
    for (const id of ids) {
      const session = sessions[id];
      if (session === undefined) {
        continue;
      }
      const next = transition(session);
      if (next !== session) {
        sessions[id] = next;
        changed = true;
      }
    }
    if (changed) {
      this.#publish({ sessions });
    }
  }

  #publish(state: AuthoringRuntimeState): void {
    this.#state = state;
    if (this.#batchDepth > 0) {
      this.#batchDirty = true;
      return;
    }
    this.#notify();
  }

  #notify(): void {
    for (const listener of this.#listeners) {
      listener();
    }
  }

  #disposeController(owned: OwnedAuthoringController): void {
    if (owned.disposed) {
      return;
    }
    owned.disposed = true;
    for (const unsubscribe of owned.bindingSubscriptions.values()) {
      unsubscribe();
    }
    owned.bindingSubscriptions.clear();
    for (const stage of owned.textStages) {
      if (!stage.closed) {
        stage.controller.dispose();
        stage.closed = true;
      }
    }
    owned.textStages.clear();
    owned.controller.dispose?.();
  }
}

/**
 * Records what a session's replica holds beyond the newest accepted state it
 * has taken, while the session has anything pending.
 */
function recordDraftOperations<TDocument>(
  owned: OwnedAuthoringController,
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  const controller = owned.controller;
  const base = owned.acceptedBaseFrontierBase64;
  if (session.policy !== "collaborative" || base === undefined || !recordsOperations(controller)) {
    return session;
  }
  if (!authoringSessionRequiresDurableRestoration(session)) {
    return withoutDraftOperations(session);
  }
  if (owned.holdsUntakenDraft) {
    return session;
  }
  const recorded = session.draftOperations;
  const updateBase64 = controller.exportIncrementalUpdateBase64(base);
  if (recorded?.baseFrontierBase64 === base && recorded.updateBase64 === updateBase64) {
    return session;
  }
  return { ...session, draftOperations: { baseFrontierBase64: base, updateBase64 } };
}

/** Whether a controller's replica already holds every operation up to a frontier. */
function holdsFrontier(
  controller: Pick<AuthoringSessionController<unknown, string>, "coversFrontierBase64">,
  frontierBase64: string
): boolean {
  return controller.coversFrontierBase64?.(frontierBase64) ?? false;
}

/** Moves a replica's accepted base to a frontier the server accepted. */
function takeAcceptedFrontier(owned: OwnedAuthoringController, acceptedFrontierBase64: string): void {
  if (owned.acceptedBaseFrontierBase64 !== undefined && recordsOperations(owned.controller)) {
    owned.acceptedBaseFrontierBase64 = acceptedFrontierBase64;
  }
}

/**
 * A session without a replica keeps its recorded operations while it is
 * pending and its draft is still the one they materialise.
 */
function withoutStaleDraftOperations<TDocument>(
  previous: AuthoringSession<unknown> | undefined,
  next: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  return previous !== undefined
    && authoringSessionRequiresDurableRestoration(next)
    && documentsEqual(previous.draft, next.draft)
    ? next
    : withoutDraftOperations(next);
}

function withoutDraftOperations<TDocument>(
  session: AuthoringSession<TDocument>
): AuthoringSession<TDocument> {
  if (session.draftOperations === undefined) {
    return session;
  }
  const next = { ...session };
  delete next.draftOperations;
  return next;
}

function cloneRuntimeState(
  state: AuthoringRuntimeState,
  recoverInterruptedReplay: boolean = false
): AuthoringRuntimeState {
  return {
    sessions: Object.fromEntries(
      Object.entries(state.sessions).map(([id, session]) => [
        id,
        {
          ...structuredClone(session),
          status:
            recoverInterruptedReplay && session.status === "replaying"
              ? "offlineModified"
              : session.status,
          queuedOperations: session.queuedOperations.map((operation) => ({
            ...structuredClone(operation),
            state:
              recoverInterruptedReplay && operation.state === "sending"
                ? "queued"
                : operation.state
          }))
        }
      ])
    )
  };
}
