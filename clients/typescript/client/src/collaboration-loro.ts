/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Plan-driven Loro replicas and text bindings. This module is the package's
 * only importer of `loro-crdt`, published as the `/loro` entry point so the
 * main entry never loads the CRDT runtime.
 */
import {
  decodeFrontiers,
  encodeFrontiers,
  LoroDoc,
  LoroList,
  LoroMap,
  LoroMovableList,
  LoroText,
  UndoManager,
  type Cursor,
  type ImportStatus,
  type LoroEventBatch
} from "loro-crdt";
import { base64ToBytes, bytesToBase64 } from "./binary";
import {
  resolveCollaborationContainer,
  type CollaborationEntityPlan,
  type CollaborationFieldPlan,
  type CollaborationPlanTextFieldPath,
  type CollaborationStorageKind
} from "./plans";
import { areJsonValuesEqual } from "./json-value-equality";
import type { AuthoringSessionController, AuthoringTextStageController } from "./authoring-session";
import { validateTextEdits, type TextBinding, type TextBindingChange, type TextEdit, type TextSelection } from "./text-binding";

const STRUCTURED_MAP_PRESENCE_CONTAINER = "collaboration.structured_map_presence";
const KEYED_SEQUENCE_PRESENCE_CONTAINER = "collaboration.keyed_sequence_presence";

type ClientDocument = object;
type ClientRecord = Record<string, unknown>;
export type CollaborationLoroDoc = LoroDoc;

/** Where a replica's initial state comes from. */
export type CollaborationReplicaSource<TDocument extends ClientDocument> =
  | { kind: "document"; document: TDocument }
  | { kind: "update"; updateBase64: string }
  | { kind: "fork"; doc: LoroDoc; document: TDocument };

/** An accepted update seeds the replica; otherwise the caller's initial content must. */
export function collaborationReplicaSource<TDocument extends ClientDocument>(
  entityName: string,
  document: TDocument | undefined,
  updateBase64: string | undefined
): CollaborationReplicaSource<TDocument> {
  if (updateBase64 !== undefined) {
    return { kind: "update", updateBase64 };
  }
  if (document === undefined) {
    throw new Error(`A new ${entityName} collaboration document requires initial content.`);
  }
  return { kind: "document", document };
}

export function requireCollaborationSchemaVersion(
  entityName: string,
  plan: CollaborationEntityPlan,
  actual: number
): void {
  if (actual !== plan.schemaVersion) {
    throw new Error(
      `Unsupported ${entityName} collaboration schema version ${actual}; expected ${plan.schemaVersion}.`
    );
  }
}

/**
 * A replica seeded from an update that depends on operations it lacks would
 * materialise a partial document, so the update is refused.
 */
function requireImportedDependencies(entityName: string, status: ImportStatus): void {
  if (status.pending !== null && status.pending.size > 0) {
    throw new Error(`The ${entityName} update depends on operations this replica does not hold.`);
  }
}

function sameFrontiers(
  left: readonly { peer: string; counter: number }[],
  right: readonly { peer: string; counter: number }[]
): boolean {
  const key = ({ peer, counter }: { peer: string; counter: number }) => `${peer}:${counter}`;
  const rightKeys = new Set(right.map(key));
  return left.length === right.length && left.every((id) => rightKeys.has(key(id)));
}

/**
 * A browser-side collaboration replica whose layout is entirely supplied by a
 * generated entity plan. {@link CollaborationDrafts} reads and writes it as an
 * application's draft.
 */
export class CollaborationLoroAuthoringDocument<TDocument extends ClientDocument> {
  private readonly doc: LoroDoc;
  /**
   * The document the containers currently hold. Never mutated in place: every
   * write or import replaces it, so a fork may share the reference.
   */
  private document: TDocument;
  private documentDirty = false;
  private readonly textBindings = new Map<string, LoroFieldTextBinding>();

  constructor(
    private readonly entityName: string,
    private readonly plan: CollaborationEntityPlan,
    source: CollaborationReplicaSource<TDocument>
  ) {
    switch (source.kind) {
      case "document": {
        const document = validatedCollaborationDocument(this.plan, source.document);
        this.doc = new LoroDoc();
        writeCollaborationDocumentChangesToLoroDoc(this.doc, this.plan, {}, document);
        this.document = document;
        return;
      }
      case "update":
        this.doc = new LoroDoc();
        requireImportedDependencies(this.entityName, this.doc.import(base64ToBytes(source.updateBase64)));
        this.document = materializeCollaborationDocumentFromLoroDoc<TDocument>(
          this.doc,
          this.plan,
          null
        );
        return;
      case "fork":
        this.doc = source.doc;
        this.document = source.document;
        return;
    }
  }

  currentDocument(): TDocument {
    this.refreshDocument();
    return structuredClone(this.document);
  }

  private refreshDocument(): void {
    this.flushTextBindings();
    if (this.documentDirty) {
      this.document = materializeCollaborationDocumentFromLoroDoc<TDocument>(this.doc, this.plan, null);
      this.documentDirty = false;
    }
  }

  private flushTextBindings(): void {
    for (const binding of this.textBindings.values()) binding.flush();
  }

  /** A stable handle to a declared text container, shared by views of this replica. */
  bindText(fieldPath: string, identities: Readonly<Record<string, string>> = {}): TextBinding {
    this.flushTextBindings();
    const target = this.resolveTextTarget(fieldPath, identities);
    const { container } = target;
    const existing = this.textBindings.get(container);
    if (existing) return existing;
    if (!target.available(this.doc)) throw new Error(`Text field ${fieldPath} belongs to a missing record.`);
    this.doc.commit({ origin: "document" });
    const origin = `text-field:${this.textBindings.size}:`;
    // A field owns its editing/history replica. Composition can hold this view
    // at its base without delaying the resource or another field.
    const fieldDoc = this.doc.fork();
    const binding = new LoroFieldTextBinding(fieldDoc, fieldDoc.getText(container), {
      origin,
      available: () => target.available(fieldDoc),
      publish: (update) => {
        this.doc.import(update);
        this.documentDirty = true;
        for (const other of this.textBindings.values()) {
          if (other !== binding) other.importUpdate(update);
        }
      },
      beforeEdit: () => {
        for (const other of this.textBindings.values()) {
          if (other !== binding) other.endHistoryGroup();
        }
      }
    });
    this.textBindings.set(container, binding);
    return binding;
  }

  /**
   * Fork a declared text target for explicit Save/Cancel editing.
   *
   * The branch shares the target's causal history but publishes nothing until
   * confirmation. Concurrent accepted edits merge when its incremental update
   * is imported. A deleted keyed record blocks confirmation and leaves the
   * branch open for recovery or copying.
   */
  stageText(
    fieldPath: string,
    identities: Readonly<Record<string, string>> = {}
  ): AuthoringTextStageController {
    this.refreshDocument();
    const target = this.resolveTextTarget(fieldPath, identities);
    if (!target.available(this.doc)) {
      throw new Error(`Text field ${fieldPath} belongs to a missing record.`);
    }
    const branch = this.fork();
    const capturedFrontier = branch.acceptedFrontierBase64();
    const binding = branch.bindText(fieldPath, identities);
    let disposed = false;

    return {
      binding,
      confirm: () => {
        if (disposed) {
          throw new Error("This staged text branch is already closed.");
        }
        if (!target.available(this.doc)) {
          return {
            kind: "blocked",
            message: "The text target was deleted while this draft was open.",
            reason: "targetDeleted"
          };
        }
        this.importUpdateBase64(
          branch.exportIncrementalUpdateBase64(capturedFrontier)
        );
        return { kind: "committed" };
      },
      dispose: () => {
        if (disposed) return;
        branch.disposeTextBindings();
        disposed = true;
      }
    };
  }

  private resolveTextTarget(
    fieldPath: string,
    identities: Readonly<Record<string, string>>
  ): {
    available: (doc: LoroDoc) => boolean;
    container: string;
  } {
    const field = this.plan.fields[fieldPath];
    const template = field?.storage.container ?? field?.storage.containerTemplate;
    if (!field || field.storage.kind !== "text" || !template) {
      throw new Error(`${this.entityName}.${fieldPath} is not a declared collaborative text field.`);
    }
    const context = { ...identities };
    const ancestors = collaborationPlanIndex(this.plan).fields.filter((candidate) =>
      candidate.storage.kind === "keyedSequence" && fieldPath.startsWith(`${candidate.path}.*.`));
    return {
      available: (doc) => ancestors.every((sequence) => {
        const variable = sequence.storage.identityVariable ?? requiredSequenceMetadata(sequence, "identityPath");
        const identity = context[variable];
        const order = resolveCollaborationContainer(requiredSequenceMetadata(sequence, "orderContainer"), context);
        return identity !== undefined && doc.getList(order).toArray().includes(identity);
      }),
      container: resolveCollaborationContainer(template, context)
    };
  }

  /** Release editor subscriptions when the owning resource is closed. */
  disposeTextBindings(): void {
    if ([...this.textBindings.values()].some((binding) => binding.composing)) {
      throw new Error("Finish text composition before closing the resource.");
    }
    this.flushTextBindings();
    for (const binding of this.textBindings.values()) binding.dispose();
    this.textBindings.clear();
  }

  /**
   * An independent replica holding the same ops and document. The fork edits
   * under its own peer, so nothing written to it reaches this replica until
   * the update is imported.
   */
  fork(): CollaborationLoroAuthoringDocument<TDocument> {
    this.refreshDocument();
    return new CollaborationLoroAuthoringDocument<TDocument>(this.entityName, this.plan, {
      kind: "fork",
      doc: this.doc.fork(),
      document: this.document
    });
  }

  /** Whether every op up to `frontierBase64` is already in this replica. */
  coversFrontierBase64(frontierBase64: string): boolean {
    this.flushTextBindings();
    const version = this.doc.oplogVersion();
    return decodeFrontiers(base64ToBytes(frontierBase64)).every(
      ({ peer, counter }) => (version.get(peer) ?? 0) > counter
    );
  }

  /** Refuses an invalid document before writing anything, leaving the replica as it was. */
  replaceDocument(document: TDocument): void {
    const next = validatedCollaborationDocument(this.plan, document);
    const before = this.doc.version();
    this.refreshDocument();
    for (const binding of this.textBindings.values()) binding.endHistoryGroup();
    writeCollaborationDocumentChangesToLoroDoc(this.doc, this.plan, this.document, next);
    this.doc.commit({ origin: "document" });
    this.document = next;
    const update = this.doc.export({ mode: "update", from: before });
    for (const binding of this.textBindings.values()) binding.importUpdate(update);
    this.documentDirty = false;
  }

  adoptDocument(document: TDocument): void {
    this.document = structuredClone(document);
    this.documentDirty = false;
  }

  /**
   * Imports an update and re-materialises only when it carried ops this
   * replica did not already hold. Returns whether the document changed.
   */
  importUpdateBase64(updateBase64: string): boolean {
    this.flushTextBindings();
    const before = this.textBindings.size > 0 ? this.doc.version() : undefined;
    const status = this.doc.import(base64ToBytes(updateBase64));
    if (status.success.size === 0) {
      return false;
    }
    this.documentDirty = true;
    // A transport message may include already-known history. Distribute only
    // the newly accepted operations and decode the wire encoding once.
    if (before) {
      const update = this.doc.export({ mode: "update", from: before });
      for (const binding of this.textBindings.values()) binding.importUpdate(update);
    }
    return true;
  }

  importVersionedUpdateBase64(schemaVersion: number, updateBase64: string): void {
    requireCollaborationSchemaVersion(this.entityName, this.plan, schemaVersion);
    this.importUpdateBase64(updateBase64);
  }

  /** Every operation this replica holds, or those up to a frontier it holds. */
  exportUpdateBase64(frontierBase64?: string): string {
    this.flushTextBindings();
    if (frontierBase64 === undefined) {
      return bytesToBase64(this.doc.export({ mode: "update" }));
    }
    if (!this.coversFrontierBase64(frontierBase64)) {
      throw new Error(`The ${this.entityName} replica does not hold that frontier.`);
    }
    const frontiers = decodeFrontiers(base64ToBytes(frontierBase64));
    if (sameFrontiers(frontiers, this.doc.oplogFrontiers())) {
      return bytesToBase64(this.doc.export({ mode: "update" }));
    }
    const fork = this.doc.forkAt(frontiers);
    try {
      return bytesToBase64(fork.export({ mode: "update" }));
    } finally {
      fork.free();
    }
  }

  exportIncrementalUpdateBase64(acceptedFrontierBase64: string): string {
    this.flushTextBindings();
    const frontiers = decodeFrontiers(base64ToBytes(acceptedFrontierBase64));
    return bytesToBase64(
      this.doc.export({
        mode: "update",
        from: this.doc.frontiersToVV(frontiers)
      })
    );
  }

  acceptedFrontierBase64(): string {
    this.flushTextBindings();
    return bytesToBase64(encodeFrontiers(this.doc.oplogFrontiers()));
  }

  /** The document as it stood at a frontier this replica holds. */
  documentAt(frontierBase64: string, revision: string | null = null): TDocument {
    if (!this.coversFrontierBase64(frontierBase64)) {
      throw new Error(`The ${this.entityName} replica does not hold that frontier.`);
    }
    const frontiers = decodeFrontiers(base64ToBytes(frontierBase64));
    if (sameFrontiers(frontiers, this.doc.oplogFrontiers())) {
      return materializeCollaborationDocumentFromLoroDoc<TDocument>(this.doc, this.plan, revision);
    }
    const fork = this.doc.forkAt(frontiers);
    try {
      return materializeCollaborationDocumentFromLoroDoc<TDocument>(fork, this.plan, revision);
    } finally {
      fork.free();
    }
  }

  materializedDocument(revision = "loro:materialized"): TDocument {
    this.flushTextBindings();
    return materializeCollaborationDocumentFromLoroDoc<TDocument>(
      this.doc,
      this.plan,
      revision
    );
  }
}

/**
 * How an application's draft of a collaborative document is read from and
 * written to the document a replica holds.
 */
export interface CollaborationDraftMapping<TDocument, TDraft> {
  /** The draft `document` holds. */
  readonly toDraft: (document: TDocument) => TDraft;
  /** `current` holding `draft` in place of its own. */
  readonly toDocument: (draft: TDraft, current: TDocument) => TDocument;
}

/** A mapping for applications that draft the document itself. */
export function documentDraftMapping<TDocument>(): CollaborationDraftMapping<TDocument, TDocument> {
  return { toDraft: (document) => document, toDocument: (draft) => draft };
}

/**
 * A replica of one collaborative document, read and written as an
 * application's draft of it.
 */
export class CollaborationDraftReplica<
  TDocument extends ClientDocument,
  TDraft,
  TTextFieldPath extends string = string
> {
  constructor(
    private readonly replica: CollaborationLoroAuthoringDocument<TDocument>,
    private readonly mapping: CollaborationDraftMapping<TDocument, TDraft>
  ) {}

  currentDraft(): TDraft {
    return this.mapping.toDraft(this.replica.currentDocument());
  }

  /** Refuses a draft whose document is invalid, leaving the replica as it was. */
  replaceDraft(draft: TDraft): void {
    this.replica.replaceDocument(this.mapping.toDocument(draft, this.replica.currentDocument()));
  }

  adoptDraft(draft: TDraft): void {
    this.replica.adoptDocument(this.mapping.toDocument(draft, this.replica.currentDocument()));
  }

  bindText(
    fieldPath: TTextFieldPath,
    identities: Readonly<Record<string, string>> = {}
  ): TextBinding {
    return this.replica.bindText(fieldPath, identities);
  }

  stageText(
    fieldPath: TTextFieldPath,
    identities: Readonly<Record<string, string>> = {}
  ): AuthoringTextStageController {
    return this.replica.stageText(fieldPath, identities);
  }

  /** An independent replica with the same operations, editing under its own peer. */
  fork(): CollaborationDraftReplica<TDocument, TDraft, TTextFieldPath> {
    return new CollaborationDraftReplica(this.replica.fork(), this.mapping);
  }

  /** Imports an update; returns whether it carried operations this replica lacked. */
  importUpdateBase64(updateBase64: string): boolean {
    return this.replica.importUpdateBase64(updateBase64);
  }

  importVersionedUpdateBase64(schemaVersion: number, updateBase64: string): void {
    this.replica.importVersionedUpdateBase64(schemaVersion, updateBase64);
  }

  exportUpdateBase64(): string {
    return this.replica.exportUpdateBase64();
  }

  exportIncrementalUpdateBase64(acceptedFrontierBase64: string): string {
    return this.replica.exportIncrementalUpdateBase64(acceptedFrontierBase64);
  }

  acceptedFrontierBase64(): string {
    return this.replica.acceptedFrontierBase64();
  }

  /** Whether every operation up to `frontierBase64` is already in this replica. */
  coversFrontierBase64(frontierBase64: string): boolean {
    return this.replica.coversFrontierBase64(frontierBase64);
  }

  /** The draft as it stood at a frontier the replica holds. */
  draftAt(frontierBase64: string, revision: string | null = null): TDraft {
    return this.mapping.toDraft(this.replica.documentAt(frontierBase64, revision));
  }

  dispose(): void {
    this.replica.disposeTextBindings();
  }

  /** This replica as the controller an authoring runtime drives. */
  controller(): AuthoringSessionController<TDraft, TTextFieldPath> {
    return {
      currentDraft: () => this.currentDraft(),
      replaceDraft: (draft) => this.replaceDraft(draft),
      adoptDocument: (draft) => this.adoptDraft(draft),
      bindText: (fieldPath, identities) => this.bindText(fieldPath, identities),
      stageText: (fieldPath, identities) => this.stageText(fieldPath, identities),
      importUpdateBase64: (updateBase64) => {
        this.importUpdateBase64(updateBase64);
      },
      importVersionedUpdateBase64: (schemaVersion, updateBase64) =>
        this.importVersionedUpdateBase64(schemaVersion, updateBase64),
      exportUpdateBase64: () => this.exportUpdateBase64(),
      exportIncrementalUpdateBase64: (acceptedFrontierBase64) =>
        this.exportIncrementalUpdateBase64(acceptedFrontierBase64),
      acceptedFrontierBase64: () => this.acceptedFrontierBase64(),
      coversFrontierBase64: (frontierBase64) => this.coversFrontierBase64(frontierBase64),
      draftAt: (frontierBase64) => this.draftAt(frontierBase64),
      dispose: () => this.dispose()
    };
  }
}

/** One collaborative entity's replicas, read and written as an application's drafts. */
export class CollaborationDrafts<
  TPlan extends CollaborationEntityPlan,
  TDocument extends ClientDocument,
  TDraft
> {
  constructor(
    readonly entity: string,
    readonly plan: TPlan,
    private readonly mapping: CollaborationDraftMapping<TDocument, TDraft>
  ) {}

  /** Refuses accepted state written under another schema version. */
  requireSchemaVersion(actual: number): void {
    requireCollaborationSchemaVersion(this.entity, this.plan, actual);
  }

  /** These drafts read and written as a further draft of their own. */
  map<TNext>(
    mapping: CollaborationDraftMapping<TDraft, TNext>
  ): CollaborationDrafts<TPlan, TDocument, TNext> {
    const inner = this.mapping;
    return new CollaborationDrafts(this.entity, this.plan, {
      toDraft: (document) => mapping.toDraft(inner.toDraft(document)),
      toDocument: (draft, current) =>
        inner.toDocument(mapping.toDocument(draft, inner.toDraft(current)), current)
    });
  }

  /** A new replica holding `document`. */
  fromDocument(
    document: TDocument
  ): CollaborationDraftReplica<TDocument, TDraft, CollaborationPlanTextFieldPath<TPlan>> {
    return this.replica({ kind: "document", document });
  }

  /** A replica holding the history an update carries. */
  fromUpdate(
    updateBase64: string
  ): CollaborationDraftReplica<TDocument, TDraft, CollaborationPlanTextFieldPath<TPlan>> {
    return this.replica({ kind: "update", updateBase64 });
  }

  private replica(
    source: CollaborationReplicaSource<TDocument>
  ): CollaborationDraftReplica<TDocument, TDraft, CollaborationPlanTextFieldPath<TPlan>> {
    return new CollaborationDraftReplica(
      new CollaborationLoroAuthoringDocument<TDocument>(this.entity, this.plan, source),
      this.mapping
    );
  }
}

/** Raw text and history objects stay inside this module. */
class LoroFieldTextBinding implements TextBinding {
  revision = 0;
  available: boolean;
  readonly origin: string;
  private readonly listeners = new Set<(change: TextBindingChange) => void>();
  private readonly undoManager: UndoManager;
  private readonly unsubscribe: () => void;
  private selectionCursors: Cursor[] = [];
  private restoredCursors: Cursor[] | null = null;
  private selectionAffinity: -1 | 1 = 1;
  private historyGroup: string | null = null;
  private disposed = false;
  private compositionDepth = 0;
  private pendingImports: Uint8Array[] = [];
  private pendingPublish = false;
  private publishedVersion: ReturnType<LoroDoc["version"]>;

  constructor(private readonly doc: LoroDoc, private readonly text: LoroText, private readonly options: {
    origin: string;
    available: () => boolean;
    publish: (update: Uint8Array) => void;
    beforeEdit: () => void;
  }) {
    this.publishedVersion = doc.version();
    this.origin = options.origin;
    this.available = options.available();
    this.undoManager = new UndoManager(doc, {
      mergeInterval: 0,
      excludeOriginPrefixes: ["document", "undo", "redo"],
      onPush: () => ({ value: this.selectionAffinity, cursors: this.selectionCursors }),
      onPop: (_undo, value) => {
        this.restoredCursors = value.cursors;
        this.selectionAffinity = value.value === -1 ? -1 : 1;
      }
    });
    this.unsubscribe = doc.subscribe((event) => this.receive(event));
  }

  get composing(): boolean { return this.compositionDepth !== 0; }
  read(): string { return this.text.toString(); }
  subscribe(listener: (change: TextBindingChange) => void): () => void {
    this.requireAvailable();
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }
  endHistoryGroup(): void {
    if (this.historyGroup !== null) this.undoManager.groupEnd();
    this.historyGroup = null;
  }
  beginComposition(): () => void {
    this.requireAvailable();
    this.endHistoryGroup();
    this.compositionDepth++;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      if (--this.compositionDepth !== 0) return;
      const pending = this.pendingImports;
      this.pendingImports = [];
      for (const update of pending) this.importUpdate(update);
    };
  }
  /** Replica exchange runs after the input transaction, or at an explicit capture boundary. */
  flush(): void {
    if (!this.pendingPublish) return;
    this.pendingPublish = false;
    const update = this.doc.export({ mode: "update", from: this.publishedVersion });
    this.publishedVersion = this.doc.version();
    this.options.publish(update);
  }
  private publishSoon(): void {
    if (this.pendingPublish) return;
    this.pendingPublish = true;
    queueMicrotask(() => this.flush());
  }
  importUpdate(update: Uint8Array): void {
    this.flush();
    if (this.compositionDepth !== 0) {
      this.pendingImports.push(update);
      return;
    }
    this.doc.import(update);
    this.publishedVersion = this.doc.version();
  }
  edit(input: Parameters<TextBinding["edit"]>[0]): void {
    this.requireAvailable();
    if (input.baseRevision !== this.revision) throw new Error("Text edit has a stale base revision.");
    validateTextEdits(this.text.length, input.changes, input.selectionBefore, input.selectionAfter);
    // Reject ranges splitting a surrogate pair before applying any operation.
    for (const change of input.changes) {
      for (const offset of [change.from, change.to]) {
        if (offset > 0 && offset < this.text.length) {
          const unicode = this.text.convertPos(offset, "utf16", "unicode");
          if (unicode === undefined || this.text.convertPos(unicode, "unicode", "utf16") !== offset) {
            throw new Error("Text range splits a Unicode character.");
          }
        }
      }
    }
    if (input.changes.length === 0) return;
    this.options.beforeEdit();
    if (this.historyGroup !== input.group) {
      this.endHistoryGroup();
      this.undoManager.groupStart();
      this.historyGroup = input.group;
    }
    this.captureSelection(input.selectionBefore);
    for (const change of [...input.changes].reverse()) {
      if (change.to > change.from) this.text.delete(change.from, change.to - change.from);
      if (change.insert) this.text.insert(change.from, change.insert);
    }
    this.doc.commit({ origin: this.origin });
    this.publishSoon();
  }
  undo(selection: TextSelection): TextSelection | null { return this.history(false, selection); }
  redo(selection: TextSelection): TextSelection | null { return this.history(true, selection); }
  dispose(): void {
    if (this.compositionDepth !== 0) throw new Error("Finish text composition before closing the resource.");
    this.flush();
    this.endHistoryGroup();
    this.unsubscribe();
    this.undoManager.free();
    this.listeners.clear();
    this.disposed = true;
  }
  private requireAvailable(): void {
    if (this.disposed || !this.available) throw new Error("The bound text field is no longer available.");
  }
  private captureSelection(selection: TextSelection): void {
    this.selectionAffinity = selection.affinity ?? 1;
    this.selectionCursors = [selection.anchor, selection.head].map((offset) => {
      const cursor = this.text.getCursor(offset, this.selectionAffinity);
      if (!cursor) throw new Error(`Cannot anchor text selection at ${offset} of ${this.text.length}.`);
      return cursor;
    });
  }
  private history(redo: boolean, selection: TextSelection): TextSelection | null {
    this.requireAvailable();
    validateTextEdits(this.text.length, [], selection, selection);
    this.options.beforeEdit();
    this.endHistoryGroup();
    this.captureSelection(selection);
    this.restoredCursors = null;
    const changed = redo ? this.undoManager.redo() : this.undoManager.undo();
    if (!changed) return null;
    this.publishSoon();
    return this.resolveHistorySelection();
  }
  private resolveHistorySelection(): TextSelection {
    const cursors = this.restoredCursors;
    if (!cursors || cursors.length !== 2) throw new Error("Text history did not retain its selection.");
    const positions = cursors.map((cursor) => this.doc.getCursorPos(cursor));
    if (!positions[0] || !positions[1]) throw new Error("Text history selection could not be resolved.");
    return { anchor: positions[0].offset, head: positions[1].offset, affinity: this.selectionAffinity };
  }
  private receive(event: LoroEventBatch): void {
    const available = this.options.available();
    const changes: TextEdit[] = [];
    for (const item of event.events) {
      if (item.target !== this.text.id || item.diff.type !== "text") continue;
      let offset = 0;
      for (const part of item.diff.diff) {
        if (part.retain !== undefined) offset += part.retain;
        else if (part.delete !== undefined) {
          changes.push({ from: offset, to: offset + part.delete, insert: "" });
          offset += part.delete;
        } else if (part.insert !== undefined) {
          changes.push({ from: offset, to: offset, insert: part.insert });
        }
      }
    }
    if (changes.length === 0 && available === this.available) return;
    if (event.by === "import") this.endHistoryGroup();
    this.available = available;
    this.revision++;
    const change: TextBindingChange = {
      revision: this.revision, available, changes,
      origin: event.by === "import" ? "remote"
        : event.origin === "undo" || event.origin === "redo" ? "history" : "local"
    };
    for (const listener of this.listeners) listener(change);
  }
}

export function collaborationDocumentToLoroDoc<TDocument extends ClientDocument>(
  plan: CollaborationEntityPlan,
  document: TDocument
): LoroDoc {
  const validated = validatedCollaborationDocument(plan, document);
  const doc = new LoroDoc();
  writeCollaborationDocumentChangesToLoroDoc(doc, plan, {}, validated);
  return doc;
}

export function materializeCollaborationDocumentFromLoroDoc<
  TDocument extends ClientDocument
>(
  doc: LoroDoc,
  plan: CollaborationEntityPlan,
  revision: string | null = "loro:materialized"
): TDocument {
  const index = collaborationPlanIndex(plan);
  const document: ClientRecord = {};
  for (const field of index.rootFields) {
    if (field.storage.kind === "derivedIdentity") {
      continue;
    }
    if (field.storage.kind === "derivedRevision") {
      if (revision !== null) {
        setClientValueAtPath(document, field.path, revision);
      }
      continue;
    }
    const value = readCollaborationField(doc, field, {});
    if (
      value !== undefined
      && !(
        isEmptyOptionalValue(value)
        && !field.required
        && !isObjectStorageKind(field.storage.kind)
      )
    ) {
      setClientValueAtPath(document, field.path, value);
    }
  }

  for (const sequence of index.rootSequences) {
    const items = materializeCollaborationSequence(doc, index, sequence, {});
    if (sequence.required || items.length > 0 || isSequenceExplicitlyPresent(doc, sequence, {})) {
      setClientValueAtPath(document, sequence.path, items);
    }
  }
  completePresentOptionalGroups(index.fields, document);
  validateCollaborationDocument(plan, document);
  return document as TDocument;
}

function completePresentOptionalGroups(
  fields: readonly CollaborationFieldPlan[],
  document: ClientRecord
): void {
  for (const field of fields) {
    if (field.required || field.path.includes("*")) {
      continue;
    }
    if (clientValueAtPath(document, field.path) !== undefined) {
      continue;
    }
    const separator = field.path.lastIndexOf(".");
    if (separator < 0) {
      continue;
    }
    const parentPath = field.path.slice(0, separator);
    if (clientValueAtPath(document, parentPath) === undefined) {
      continue;
    }
    if (field.value.codec === "optionalString") {
      setClientValueAtPath(document, field.path, "");
    } else if (
      field.value.codec === "stringList"
      || (
        field.value.codec === "structuredJson"
        && !isObjectStorageKind(field.storage.kind)
      )
    ) {
      setClientValueAtPath(document, field.path, []);
    }
  }
}

function isEmptyOptionalValue(value: unknown): boolean {
  return value === null
    || value === ""
    || (Array.isArray(value) && value.length === 0)
    || (isRecord(value) && Object.keys(value).length === 0);
}

/** Writes the changes from `previous` to `next`, which the caller has validated. */
function writeCollaborationDocumentChangesToLoroDoc<
  TDocument extends ClientDocument
>(
  doc: LoroDoc,
  plan: CollaborationEntityPlan,
  previous: ClientDocument,
  next: TDocument
): void {
  const index = collaborationPlanIndex(plan);
  for (const field of index.rootFields) {
    if (isDerivedField(field)) {
      continue;
    }
    const previousValue = clientValueAtPath(previous, field.path);
    const nextValue = clientValueAtPath(next, field.path);
    if (!areWireValuesEqual(previousValue, nextValue)) {
      writeCollaborationField(doc, field, previousValue, nextValue, {});
    }
  }

  for (const sequence of index.rootSequences) {
    writeCollaborationSequence(doc, index, sequence, previous, next, {});
  }
}

function materializeCollaborationSequence(
  doc: LoroDoc,
  index: CollaborationPlanIndex,
  sequence: CollaborationFieldPlan,
  parentContext: Readonly<Record<string, string>>
): ClientRecord[] {
  const identityPath = requiredSequenceMetadata(sequence, "identityPath");
  const identityVariable = sequence.storage.identityVariable ?? identityPath;
  const orderContainer = resolveCollaborationContainer(
    requiredSequenceMetadata(sequence, "orderContainer"),
    parentContext
  );
  const identities = uniqueStableIdentities(
    stringArray(sequence, doc.getList(orderContainer).toArray())
  );
  const itemFields = index.itemFields(sequence);
  const childSequences = index.childSequences(sequence);
  return identities.map((identity) => {
    const item: ClientRecord = {};
    const context = { ...parentContext, [identityVariable]: identity };
    for (const field of itemFields) {
      const itemPath = field.path.slice(`${sequence.path}.*.`.length);
      if (field.storage.kind === "derivedIdentity") {
        const variable = field.storage.identityVariable ?? field.storage.identityPath ?? identityVariable;
        const derivedIdentity = context[variable];
        if (derivedIdentity === undefined) {
          throw new Error(`Collaboration identity ${variable} is missing for ${field.path}.`);
        }
        setClientValueAtPath(item, itemPath, derivedIdentity);
        continue;
      }
      if (field.storage.kind === "derivedRevision") {
        continue;
      }
      const value = readCollaborationField(doc, field, context);
      if (
        value !== undefined
        && !(
          isEmptyOptionalValue(value)
          && !field.required
          && !isObjectStorageKind(field.storage.kind)
        )
      ) {
        setClientValueAtPath(item, itemPath, value);
      }
    }
    for (const child of childSequences) {
      const childPath = child.path.slice(`${sequence.path}.*.`.length);
      const childItems = materializeCollaborationSequence(doc, index, child, context);
      if (child.required || childItems.length > 0 || isSequenceExplicitlyPresent(doc, child, context)) {
        setClientValueAtPath(item, childPath, childItems);
      }
    }
    return item;
  });
}

function writeCollaborationSequence(
  doc: LoroDoc,
  index: CollaborationPlanIndex,
  sequence: CollaborationFieldPlan,
  previousParent: unknown,
  nextParent: unknown,
  parentContext: Readonly<Record<string, string>>
): void {
  const relativePath = index.relativeSequencePath(sequence);
  const nextValue = clientValueAtPath(nextParent, relativePath);
  const nextItems = isAbsent(nextValue) ? [] : nextValue;
  if (!Array.isArray(nextItems)) {
    throw new Error(`Collaboration field ${sequence.path} must be an array.`);
  }
  const previousValue = clientValueAtPath(previousParent, relativePath);
  const previousItems = Array.isArray(previousValue) ? previousValue : [];
  const identityPath = requiredSequenceMetadata(sequence, "identityPath");
  const identityVariable = sequence.storage.identityVariable ?? identityPath;
  const identities = nextItems.map((item) => requiredClientIdentity(item, identityPath));
  const previousIdentities = previousItems.map((item) => requiredClientIdentity(item, identityPath));
  const orderContainer = resolveCollaborationContainer(
    requiredSequenceMetadata(sequence, "orderContainer"),
    parentContext
  );
  const presence = doc.getMap(KEYED_SEQUENCE_PRESENCE_CONTAINER);
  if (!isAbsent(nextValue) || sequence.required) {
    presence.set(orderContainer, true);
  } else if (presence.get(orderContainer) !== undefined) {
    presence.delete(orderContainer);
  }
  if (!areWireValuesEqual(previousIdentities, identities)) {
    setStringList(doc.getList(orderContainer), identities);
  }
  const previousByIdentity = new Map(
    previousItems.map((item) => [requiredClientIdentity(item, identityPath), item])
  );
  const itemFields = index.itemFields(sequence);
  const childSequences = index.childSequences(sequence);
  nextItems.forEach((item, position) => {
    const identity = identities[position]!;
    const previousItem = previousByIdentity.get(identity);
    // An item equal to what the containers already hold has no field or
    // child-sequence change to write; the cost of a save is then bounded by
    // the edit, not the document.
    if (previousItem !== undefined && areWireValuesEqual(previousItem, item)) {
      return;
    }
    const context = { ...parentContext, [identityVariable]: identity };
    for (const field of itemFields) {
      if (isDerivedField(field)) {
        continue;
      }
      const itemPath = field.path.slice(`${sequence.path}.*.`.length);
      const previousFieldValue = clientValueAtPath(previousItem, itemPath);
      const nextFieldValue = clientValueAtPath(item, itemPath);
      if (!areWireValuesEqual(previousFieldValue, nextFieldValue)) {
        writeCollaborationField(
          doc,
          field,
          previousFieldValue,
          nextFieldValue,
          context
        );
      }
    }
    for (const child of childSequences) {
      writeCollaborationSequence(doc, index, child, previousItem, item, context);
    }
  });
}

function isSequenceExplicitlyPresent(
  doc: LoroDoc,
  sequence: CollaborationFieldPlan,
  context: Readonly<Record<string, string>>
): boolean {
  const orderContainer = resolveCollaborationContainer(
    requiredSequenceMetadata(sequence, "orderContainer"),
    context
  );
  return doc.getMap(KEYED_SEQUENCE_PRESENCE_CONTAINER).get(orderContainer) !== undefined;
}

/**
 * The plan's structure, resolved once per plan. Every sequence write and read
 * used to re-filter the whole field list per item, which made a one-field edit
 * cost as much as the document is large.
 */
interface CollaborationPlanIndex {
  readonly fields: readonly CollaborationFieldPlan[];
  /** Non-sequence fields outside any sequence. */
  readonly rootFields: readonly CollaborationFieldPlan[];
  readonly rootSequences: readonly CollaborationFieldPlan[];
  /** Non-sequence fields directly inside one item of `sequence`. */
  itemFields(sequence: CollaborationFieldPlan): readonly CollaborationFieldPlan[];
  /** Sequences directly inside one item of `sequence`. */
  childSequences(sequence: CollaborationFieldPlan): readonly CollaborationFieldPlan[];
  /** The sequence's path relative to its parent item, or its full path at the root. */
  relativeSequencePath(sequence: CollaborationFieldPlan): string;
}

const PLAN_INDEXES = new WeakMap<CollaborationEntityPlan, CollaborationPlanIndex>();

function collaborationPlanIndex(plan: CollaborationEntityPlan): CollaborationPlanIndex {
  const cached = PLAN_INDEXES.get(plan);
  if (cached !== undefined) {
    return cached;
  }
  const fields = collaborationFields(plan);
  const sequences = fields.filter((field) => field.storage.kind === "keyedSequence");
  const isDirectlyUnder = (field: CollaborationFieldPlan, parentPath: string | null): boolean => {
    if (parentPath === null) {
      return !field.path.includes(".*.");
    }
    const prefix = `${parentPath}.*.`;
    return field.path.startsWith(prefix) && !field.path.slice(prefix.length).includes(".*.");
  };
  const itemFields = new Map<string, readonly CollaborationFieldPlan[]>();
  const childSequences = new Map<string, readonly CollaborationFieldPlan[]>();
  const relativePaths = new Map<string, string>();
  for (const sequence of sequences) {
    itemFields.set(
      sequence.path,
      fields.filter((field) =>
        field.storage.kind !== "keyedSequence" && isDirectlyUnder(field, sequence.path)
      )
    );
    childSequences.set(
      sequence.path,
      sequences.filter((field) => isDirectlyUnder(field, sequence.path))
    );
    const parent = sequences
      .filter((candidate) => sequence.path.startsWith(`${candidate.path}.*.`))
      .sort((left, right) => right.path.length - left.path.length)[0];
    relativePaths.set(
      sequence.path,
      parent === undefined ? sequence.path : sequence.path.slice(`${parent.path}.*.`.length)
    );
  }
  const requireEntry = <T>(map: ReadonlyMap<string, T>, sequence: CollaborationFieldPlan): T => {
    const entry = map.get(sequence.path);
    if (entry === undefined) {
      throw new Error(`Collaboration field ${sequence.path} is not a keyed sequence of this plan.`);
    }
    return entry;
  };
  const index: CollaborationPlanIndex = {
    fields,
    rootFields: fields.filter((field) =>
      field.storage.kind !== "keyedSequence" && !field.path.includes("*")
    ),
    rootSequences: sequences.filter((field) => isDirectlyUnder(field, null)),
    itemFields: (sequence) => requireEntry(itemFields, sequence),
    childSequences: (sequence) => requireEntry(childSequences, sequence),
    relativeSequencePath: (sequence) => requireEntry(relativePaths, sequence)
  };
  PLAN_INDEXES.set(plan, index);
  return index;
}

function requiredSequenceMetadata(
  sequence: CollaborationFieldPlan,
  key: "identityPath" | "orderContainer"
): string {
  const value = sequence.storage[key];
  if (value === undefined) {
    throw new Error(`Keyed collaboration sequence ${sequence.path} is missing ${key}.`);
  }
  return value;
}

function collaborationFields(plan: CollaborationEntityPlan): readonly CollaborationFieldPlan[] {
  return Object.values(plan.fields);
}

function readCollaborationField(
  doc: LoroDoc,
  field: CollaborationFieldPlan,
  identities: Readonly<Record<string, string>>
): unknown {
  const containerTemplate = field.storage.container ?? field.storage.containerTemplate;
  if (containerTemplate === undefined) {
    throw new Error(`Collaboration field ${field.path} has no container.`);
  }
  const container = resolveCollaborationContainer(containerTemplate, identities);
  switch (field.storage.kind) {
    case "scalar": {
      const key = field.storage.key;
      if (key === undefined) {
        throw new Error(`Scalar collaboration field ${field.path} has no key.`);
      }
      return doc.getMap(container).get(key);
    }
    case "text": {
      const value = doc.getText(container).toString();
      if (field.value.codec === "propertyText") {
        const metadataContainerTemplate =
          field.storage.metadataContainer ?? field.storage.metadataContainerTemplate;
        const metadataKey = field.storage.metadataKey;
        if (metadataContainerTemplate === undefined || metadataKey === undefined) {
          throw new Error(`Property text collaboration field ${field.path} has no MIME storage.`);
        }
        const metadataContainer = resolveCollaborationContainer(
          metadataContainerTemplate,
          identities
        );
        const mime = doc.getMap(metadataContainer).get(metadataKey);
        if (mime === undefined && !field.required) {
          return undefined;
        }
        if (typeof mime !== "string") {
          throw new Error(`Property text collaboration field ${field.path} has no MIME value.`);
        }
        return { "$mime": mime, value };
      }
      return value.length === 0 && field.value.codec === "optionalString"
        ? undefined
        : value;
    }
    case "orderedList":
      return doc.getList(container).toArray();
    case "structuredList":
      return doc.getList(container).toArray().map((entry) =>
        typeof entry === "object" && entry !== null
          ? mapObjectKeys(entry as ClientRecord, snakeToCamel)
          : entry
      );
    case "structuredMap": {
      const value = doc.getMap(container).toJSON();
      const explicitlyPresent = doc.getMap(STRUCTURED_MAP_PRESENCE_CONTAINER).get(container) !== undefined;
      return field.required || explicitlyPresent || Object.keys(value).length > 0
        ? value
        : undefined;
    }
    case "structuredDocument": {
      const value = readStructuredDocument(doc.getMap(container));
      const explicitlyPresent = doc.getMap(STRUCTURED_MAP_PRESENCE_CONTAINER).get(container) !== undefined;
      return field.required || explicitlyPresent || Object.keys(value).length > 0
        ? value
        : undefined;
    }
    case "derivedIdentity":
    case "derivedRevision":
    case "keyedSequence":
      return undefined;
  }
}

function writeCollaborationField(
  doc: LoroDoc,
  field: CollaborationFieldPlan,
  previousValue: unknown,
  value: unknown,
  identities: Readonly<Record<string, string>>
): void {
  const containerTemplate = field.storage.container ?? field.storage.containerTemplate;
  if (containerTemplate === undefined) {
    throw new Error(`Collaboration field ${field.path} has no container.`);
  }
  const container = resolveCollaborationContainer(containerTemplate, identities);
  switch (field.storage.kind) {
    case "scalar": {
      const key = field.storage.key;
      if (key === undefined) {
        throw new Error(`Scalar collaboration field ${field.path} has no key.`);
      }
      const map = doc.getMap(container);
      if (isAbsent(value) || (field.value.codec === "optionalString" && value === "")) {
        deletePresentKey(map, key);
      } else {
        map.set(key, scalarValue(field, value));
      }
      return;
    }
    case "text":
      setText(doc.getText(container), isAbsent(value) ? "" : textValue(field, value));
      if (field.value.codec === "propertyText") {
        const metadataContainerTemplate =
          field.storage.metadataContainer ?? field.storage.metadataContainerTemplate;
        const metadataKey = field.storage.metadataKey;
        if (metadataContainerTemplate === undefined || metadataKey === undefined) {
          throw new Error(`Property text collaboration field ${field.path} has no MIME storage.`);
        }
        const metadata = doc.getMap(resolveCollaborationContainer(metadataContainerTemplate, identities));
        if (isAbsent(value)) {
          deletePresentKey(metadata, metadataKey);
        } else {
          metadata.set(metadataKey, propertyTextValue(field, value)["$mime"]);
        }
      }
      return;
    case "orderedList":
      setStringList(doc.getList(container), isAbsent(value) ? [] : stringArray(field, value));
      return;
    case "structuredList":
      setStructuredList(doc.getList(container), isAbsent(value) ? [] : structuredArray(field, value));
      return;
    case "structuredMap":
      if (isAbsent(value)) {
        setStructuredMap(
          doc.getMap(container),
          isRecord(previousValue) ? previousValue : {},
          {}
        );
        deleteStructuredPresence(doc, container);
      } else {
        setStructuredMap(
          doc.getMap(container),
          isRecord(previousValue) ? previousValue : {},
          structuredRecord(field, value)
        );
        doc.getMap(STRUCTURED_MAP_PRESENCE_CONTAINER).set(container, true);
      }
      return;
    case "structuredDocument":
      if (isAbsent(value)) {
        setStructuredDocument(
          doc.getMap(container),
          isRecord(previousValue) ? previousValue : {},
          {}
        );
        deleteStructuredPresence(doc, container);
      } else {
        setStructuredDocument(
          doc.getMap(container),
          isRecord(previousValue) ? previousValue : {},
          structuredRecord(field, value)
        );
        doc.getMap(STRUCTURED_MAP_PRESENCE_CONTAINER).set(container, true);
      }
      return;
    case "derivedIdentity":
    case "derivedRevision":
    case "keyedSequence":
      return;
  }
}

function deleteStructuredPresence(doc: LoroDoc, container: string): void {
  deletePresentKey(doc.getMap(STRUCTURED_MAP_PRESENCE_CONTAINER), container);
}

/** Deleting an absent key still records an op, so only delete what is present. */
function deletePresentKey(map: LoroMap, key: string): void {
  if (map.get(key) !== undefined) {
    map.delete(key);
  }
}

/** Absent and `null` are the same thing: no value. */
function isAbsent(value: unknown): value is undefined | null {
  return value === undefined || value === null;
}

function isDerivedField(field: CollaborationFieldPlan): boolean {
  return field.storage.kind === "derivedIdentity" || field.storage.kind === "derivedRevision";
}

function scalarValue(
  field: CollaborationFieldPlan,
  value: unknown
): string | number | boolean {
  if (field.value.codec === "integer") {
    if (!Number.isInteger(value)) {
      throw new Error(`Collaboration field ${field.path} must be an integer.`);
    }
    return value as number;
  }
  if (field.value.codec === "number" || field.value.codec === "optionalNumber") {
    if (typeof value !== "number" || !Number.isFinite(value)) {
      throw new Error(`Collaboration field ${field.path} must be a finite number.`);
    }
    return value;
  }
  if (field.value.codec === "boolean") {
    if (typeof value !== "boolean") {
      throw new Error(`Collaboration field ${field.path} must be a boolean.`);
    }
    return value;
  }
  if (typeof value !== "string") {
    throw new Error(`Collaboration field ${field.path} must be a string.`);
  }
  return value;
}

function textValue(field: CollaborationFieldPlan, value: unknown): string {
  if (field.value.codec === "propertyText") {
    return propertyTextValue(field, value).value;
  }
  if (typeof value !== "string") {
    throw new Error(`Collaboration field ${field.path} must be a string.`);
  }
  return value;
}

/** A `propertyText` value: text tagged with a `text/*` MIME type. */
interface PropertyTextValue {
  readonly "$mime": string;
  readonly value: string;
}

function propertyTextValue(field: CollaborationFieldPlan, value: unknown): PropertyTextValue {
  if (
    typeof value !== "object"
    || value === null
    || !("$mime" in value)
    || !("value" in value)
  ) {
    throw new Error(`Collaboration field ${field.path} must be a property value.`);
  }
  const propertyValue = value as { readonly "$mime": unknown; readonly value: unknown };
  if (
    typeof propertyValue["$mime"] !== "string"
    || !propertyValue["$mime"].startsWith("text/")
    || typeof propertyValue.value !== "string"
  ) {
    throw new Error(`Collaboration field ${field.path} must be a text property value.`);
  }
  return { "$mime": propertyValue["$mime"], value: propertyValue.value };
}

function stringArray(field: CollaborationFieldPlan, value: unknown): string[] {
  if (!Array.isArray(value) || !value.every((entry) => typeof entry === "string")) {
    throw new Error(`Collaboration field ${field.path} must be a string array.`);
  }
  return value;
}

function structuredArray(
  field: CollaborationFieldPlan,
  value: unknown
): ReadonlyArray<ClientRecord> {
  if (!Array.isArray(value) || !value.every(isRecord)) {
    throw new Error(`Collaboration field ${field.path} must be an object array.`);
  }
  return value.map((entry) => mapObjectKeys(entry, camelToSnake));
}

function setStructuredList(list: LoroList, values: ReadonlyArray<ClientRecord>): void {
  writeList(list, values);
}

function structuredRecord(
  field: CollaborationFieldPlan,
  value: unknown
): ClientRecord {
  if (!isRecord(value)) {
    throw new Error(`Collaboration field ${field.path} must be an object.`);
  }
  return value;
}

function setStructuredMap(
  map: LoroMap,
  previous: ClientRecord,
  next: ClientRecord
): void {
  const nextKeys = new Set(Object.keys(next));
  for (const key of Object.keys(previous)) {
    if (!nextKeys.has(key)) {
      map.delete(key);
    }
  }
  for (const [key, nextEntry] of Object.entries(next)) {
    const previousEntry = previous[key];
    if (areWireValuesEqual(previousEntry, nextEntry)) {
      continue;
    }
    if (isRecord(nextEntry)) {
      const child = ensureStructuredMapChild(map, key);
      setStructuredMap(child, isRecord(previousEntry) ? previousEntry : {}, nextEntry);
    } else {
      map.set(key, structuredClone(nextEntry));
    }
  }
}

/**
 * Reserved keys describing an identity-keyed list inside a structured
 * document. A plain object carrying `$keyedBy` is stored as an opaque value
 * instead, so the reader can never mistake authored data for this shape.
 */
const KEYED_LIST_MARKER = "$keyedBy";
const KEYED_LIST_ORDER = "$order";
const KEYED_LIST_ITEMS = "$items";
const KEYED_LIST_IDENTITIES = ["id", "uid"] as const;

function isObjectStorageKind(kind: CollaborationStorageKind): boolean {
  return kind === "structuredMap" || kind === "structuredDocument";
}

/** The identity key a list of objects can be keyed by. */
function keyedListIdentity(values: readonly unknown[]): string | undefined {
  if (values.length === 0) {
    return undefined;
  }
  return KEYED_LIST_IDENTITIES.find((identity) => {
    const seen = new Set<string>();
    return values.every((value) => {
      if (!isRecord(value) || KEYED_LIST_MARKER in value) {
        return false;
      }
      const key = value[identity];
      return typeof key === "string" && key.length > 0 && !seen.has(key) && seen.add(key) !== undefined;
    });
  });
}

/**
 * Writes one structured document level.
 *
 * Strings become text containers so concurrent typing merges by character;
 * lists whose elements carry a stable identity become keyed so two writers
 * adding entries both keep theirs. Everything else stays a plain value, which
 * is last-writer-wins.
 */
function setStructuredDocument(
  map: LoroMap,
  previous: ClientRecord,
  next: ClientRecord
): void {
  const nextKeys = new Set(Object.keys(next));
  for (const key of Object.keys(previous)) {
    if (!nextKeys.has(key)) {
      map.delete(key);
    }
  }
  for (const [key, nextEntry] of Object.entries(next)) {
    const previousEntry = previous[key];
    if (areWireValuesEqual(previousEntry, nextEntry)) {
      continue;
    }
    if (typeof nextEntry === "string") {
      // A diff-based update, so a concurrent edit to the same prose merges
      // instead of replacing the other writer's text.
      ensureStructuredDocumentText(map, key).update(nextEntry);
      continue;
    }
    if (Array.isArray(nextEntry)) {
      const identity = keyedListIdentity(nextEntry);
      if (identity !== undefined) {
        setKeyedList(
          ensureStructuredMapChild(map, key),
          identity,
          Array.isArray(previousEntry) ? previousEntry : [],
          nextEntry
        );
      } else {
        map.set(key, structuredClone(nextEntry));
      }
      continue;
    }
    if (isRecord(nextEntry) && !(KEYED_LIST_MARKER in nextEntry)) {
      setStructuredDocument(
        ensureStructuredMapChild(map, key),
        isRecord(previousEntry) ? previousEntry : {},
        nextEntry
      );
      continue;
    }
    map.set(key, structuredClone(nextEntry));
  }
}

function setKeyedList(
  map: LoroMap,
  identity: string,
  previous: readonly unknown[],
  next: readonly unknown[]
): void {
  map.set(KEYED_LIST_MARKER, identity);
  const items = ensureStructuredMapChild(map, KEYED_LIST_ITEMS);
  const previousByIdentity = listByIdentity(previous, identity);
  const nextByIdentity = listByIdentity(next, identity);

  for (const key of Object.keys(previousByIdentity)) {
    if (!(key in nextByIdentity)) {
      items.delete(key);
    }
  }
  for (const [key, value] of Object.entries(nextByIdentity)) {
    setStructuredDocument(
      ensureStructuredMapChild(items, key),
      previousByIdentity[key] ?? {},
      value
    );
  }

  setStringList(
    map.ensureMergeableList(KEYED_LIST_ORDER) as LoroList,
    next
      .map((value) => (isRecord(value) ? value[identity] : undefined))
      .filter((key): key is string => typeof key === "string")
  );
}

function listByIdentity(
  values: readonly unknown[],
  identity: string
): Record<string, ClientRecord> {
  const byIdentity: Record<string, ClientRecord> = {};
  for (const value of values) {
    if (!isRecord(value)) {
      continue;
    }
    const key = value[identity];
    if (typeof key === "string" && key.length > 0) {
      byIdentity[key] = value;
    }
  }
  return byIdentity;
}

function ensureStructuredDocumentText(map: LoroMap, key: string): LoroText {
  const existing = map.entries().find(([entryKey]) => entryKey === key)?.[1];
  if (existing !== undefined && !(existing instanceof LoroText)) {
    map.delete(key);
  }
  return map.ensureMergeableText(key);
}

/** Rebuilds one structured document level from its containers. */
function readStructuredDocument(map: LoroMap): ClientRecord {
  const values: ClientRecord = {};
  for (const [key, entry] of map.entries()) {
    if (entry instanceof LoroText) {
      values[key] = entry.toString();
    } else if (entry instanceof LoroMap) {
      values[key] = entry.get(KEYED_LIST_MARKER) !== undefined
        ? readKeyedList(entry)
        : readStructuredDocument(entry);
    } else if (entry instanceof LoroList || entry instanceof LoroMovableList) {
      values[key] = entry.toJSON();
    } else {
      values[key] = entry;
    }
  }
  return values;
}

function readKeyedList(map: LoroMap): unknown[] {
  const identity = map.get(KEYED_LIST_MARKER);
  if (typeof identity !== "string") {
    return [];
  }
  const items = map.get(KEYED_LIST_ITEMS);
  if (!(items instanceof LoroMap)) {
    return [];
  }
  const order = map.get(KEYED_LIST_ORDER);
  const declaredOrder = order instanceof LoroList
    ? order.toArray().filter((value): value is string => typeof value === "string")
    : [];

  // Order first, then anything an order rewrite lost: a concurrently added
  // item keeps its place in the document even when the two order lists
  // disagreed.
  const itemKeys = items.keys().slice().sort();
  const seen = new Set<string>();
  const ordered: unknown[] = [];
  for (const key of [...declaredOrder, ...itemKeys]) {
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    const child = items.get(key);
    if (!(child instanceof LoroMap)) {
      continue;
    }
    ordered.push({ ...readStructuredDocument(child), [identity]: key });
  }
  return ordered;
}

function ensureStructuredMapChild(map: LoroMap, key: string): LoroMap {
  const existing = map.entries().find(([entryKey]) => entryKey === key)?.[1];
  if (existing !== undefined && !(existing instanceof LoroMap)) {
    map.delete(key);
  }
  return map.ensureMergeableMap(key);
}

/** Plan paths are a small fixed set, so their client-side segments are split once. */
const CLIENT_PATH_SEGMENTS = new Map<string, readonly string[]>();

function clientPathSegments(path: string): readonly string[] {
  let segments = CLIENT_PATH_SEGMENTS.get(path);
  if (segments === undefined) {
    segments = path.split(".").map(snakeToCamel);
    CLIENT_PATH_SEGMENTS.set(path, segments);
  }
  return segments;
}

function clientValueAtPath(value: unknown, path: string): unknown {
  let current = value;
  for (const segment of clientPathSegments(path)) {
    if (!isRecord(current)) {
      return undefined;
    }
    current = current[segment];
  }
  return current;
}

function setClientValueAtPath(root: ClientRecord, path: string, value: unknown): void {
  const segments = [...clientPathSegments(path)];
  const last = segments.pop();
  if (last === undefined) {
    throw new Error("Collaboration field path must not be empty.");
  }
  let current = root;
  for (const segment of segments) {
    const child = current[segment];
    if (!isRecord(child)) {
      current[segment] = {};
    }
    current = current[segment] as ClientRecord;
  }
  current[last] = value;
}

/**
 * A copy of `document` checked whole against its plan, so a write never starts
 * on a document it would have to refuse part-way through. Optional fields set
 * to null are dropped: the write removes them.
 */
function validatedCollaborationDocument<TDocument extends ClientDocument>(
  plan: CollaborationEntityPlan,
  document: TDocument
): TDocument {
  const copy = structuredClone(document);
  validateCollaborationDocument(plan, copy);
  const index = collaborationPlanIndex(plan);
  for (const field of index.rootFields) {
    deleteNullAtClientPath(copy, field.path);
  }
  for (const sequence of index.rootSequences) {
    deleteNullSequenceFields(index, sequence, copy);
  }
  return copy;
}

function deleteNullSequenceFields(
  index: CollaborationPlanIndex,
  sequence: CollaborationFieldPlan,
  parent: unknown
): void {
  const relativePath = index.relativeSequencePath(sequence);
  const items = clientValueAtPath(parent, relativePath);
  if (!Array.isArray(items)) {
    deleteNullAtClientPath(parent, relativePath);
    return;
  }
  for (const item of items) {
    for (const field of index.itemFields(sequence)) {
      deleteNullAtClientPath(item, field.path.slice(`${sequence.path}.*.`.length));
    }
    for (const child of index.childSequences(sequence)) {
      deleteNullSequenceFields(index, child, item);
    }
  }
}

function deleteNullAtClientPath(root: unknown, path: string): void {
  const segments = clientPathSegments(path);
  let parent = root;
  for (const segment of segments.slice(0, -1)) {
    if (!isRecord(parent)) {
      return;
    }
    parent = parent[segment];
  }
  const last = segments[segments.length - 1];
  if (isRecord(parent) && last !== undefined && parent[last] === null) {
    delete parent[last];
  }
}

function validateCollaborationDocument(
  plan: CollaborationEntityPlan,
  value: unknown
): void {
  if (!isRecord(value)) {
    throw new Error("A collaboration document must be an object.");
  }
  const index = collaborationPlanIndex(plan);
  for (const field of index.rootFields) {
    if (!isDerivedField(field)) {
      validateClientFieldValue(field, clientValueAtPath(value, field.path));
    }
  }
  for (const sequence of index.rootSequences) {
    validateClientSequence(index, sequence, value, {});
  }
}

function validateClientSequence(
  index: CollaborationPlanIndex,
  sequence: CollaborationFieldPlan,
  parent: unknown,
  context: Readonly<Record<string, string>>
): void {
  const relativePath = index.relativeSequencePath(sequence);
  const items = clientValueAtPath(parent, relativePath);
  if (isAbsent(items)) {
    if (sequence.required) {
      throw new Error(`Collaboration field ${sequence.path} is required.`);
    }
    return;
  }
  if (!Array.isArray(items)) {
    throw new Error(`Collaboration field ${sequence.path} must be an array.`);
  }
  const identityPath = requiredSequenceMetadata(sequence, "identityPath");
  const identityVariable = sequence.storage.identityVariable ?? identityPath;
  const identities = new Set<string>();
  const itemFields = index.itemFields(sequence)
    .filter((itemField) => !isDerivedField(itemField))
    .map((itemField) => ({
      field: itemField,
      itemPath: itemField.path.slice(`${sequence.path}.*.`.length)
    }));
  const childSequences = index.childSequences(sequence);
  for (const item of items) {
    const identity = requiredClientIdentity(item, identityPath);
    if (identities.has(identity)) {
      throw new Error(`Collaboration field ${sequence.path} contains duplicate identity ${identity}.`);
    }
    identities.add(identity);
    const itemContext = { ...context, [identityVariable]: identity };
    for (const { field: itemField, itemPath } of itemFields) {
      validateClientFieldValue(itemField, clientValueAtPath(item, itemPath));
    }
    for (const child of childSequences) {
      validateClientSequence(index, child, item, itemContext);
    }
  }
}

/**
 * A required field must be present and satisfy its codec. An optional field
 * may be absent or null, which removes it; a present value must satisfy its
 * codec.
 */
function validateClientFieldValue(field: CollaborationFieldPlan, value: unknown): void {
  if (isAbsent(value)) {
    if (field.required) {
      throw new Error(`Collaboration field ${field.path} is required.`);
    }
    return;
  }
  switch (field.storage.kind) {
    case "scalar":
      scalarValue(field, value);
      return;
    case "text":
      textValue(field, value);
      return;
    case "orderedList":
      stringArray(field, value);
      return;
    case "structuredList":
      structuredArray(field, value);
      return;
    case "structuredMap":
    case "structuredDocument":
      structuredRecord(field, value);
      return;
    case "derivedIdentity":
    case "derivedRevision":
    case "keyedSequence":
      return;
  }
}

function requiredClientIdentity(value: unknown, identityPath: string): string {
  const identity = clientValueAtPath(value, identityPath);
  if (typeof identity !== "string" || identity.length === 0) {
    throw new Error(`Collaboration identity ${identityPath} must be a non-empty string.`);
  }
  return identity;
}

function uniqueStableIdentities(identities: readonly string[]): string[] {
  const seen = new Set<string>();
  return identities.filter((identity) => {
    if (identity.length === 0 || seen.has(identity)) {
      return false;
    }
    seen.add(identity);
    return true;
  });
}

function snakeToCamel(value: string): string {
  return value.replace(/_([a-z])/g, (_match, letter: string) => letter.toUpperCase());
}

function camelToSnake(value: string): string {
  return value.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
}

function mapObjectKeys(
  value: ClientRecord,
  mapper: (key: string) => string
): ClientRecord {
  return Object.fromEntries(
    Object.entries(value).map(([key, child]) => [
      mapper(key),
      Array.isArray(child)
        ? child.map((entry) => isRecord(entry) ? mapObjectKeys(entry, mapper) : entry)
        : isRecord(child)
          ? mapObjectKeys(child, mapper)
          : child
    ])
  );
}

function setText(text: LoroText, value: string): void {
  text.update(value);
}

function setStringList(list: LoroList, values: readonly string[]): void {
  writeList(list, values);
}

/**
 * Rewrites a list to `desired` as the deletions and insertions of a longest
 * common subsequence, so an entry both versions hold keeps its identity and an
 * entry another writer inserts concurrently survives the merge.
 */
function writeList(list: LoroList, desired: readonly unknown[]): void {
  const current = list.toJSON() as unknown[];
  let start = 0;
  while (
    start < current.length
    && start < desired.length
    && areWireValuesEqual(current[start], desired[start])
  ) {
    start += 1;
  }
  let currentEnd = current.length;
  let desiredEnd = desired.length;
  while (
    currentEnd > start
    && desiredEnd > start
    && areWireValuesEqual(current[currentEnd - 1], desired[desiredEnd - 1])
  ) {
    currentEnd -= 1;
    desiredEnd -= 1;
  }
  const rows = currentEnd - start;
  const columns = desiredEnd - start;
  const width = columns + 1;
  const same = (row: number, column: number): boolean =>
    areWireValuesEqual(current[start + row], desired[start + column]);
  // common[row * width + column] is the longest common subsequence of the
  // current entries from `row` and the desired entries from `column`.
  const common = new Uint32Array((rows + 1) * width);
  for (let row = rows - 1; row >= 0; row -= 1) {
    for (let column = columns - 1; column >= 0; column -= 1) {
      common[row * width + column] = same(row, column)
        ? common[(row + 1) * width + column + 1]! + 1
        : Math.max(common[(row + 1) * width + column]!, common[row * width + column + 1]!);
    }
  }
  let position = start;
  let row = 0;
  let column = 0;
  while (row < rows || column < columns) {
    if (row < rows && column < columns && same(row, column)) {
      position += 1;
      row += 1;
      column += 1;
    } else if (
      column < columns
      && (row === rows || common[row * width + column + 1]! >= common[(row + 1) * width + column]!)
    ) {
      list.insert(position, desired[start + column]);
      position += 1;
      column += 1;
    } else {
      list.delete(position, 1);
      row += 1;
    }
  }
}

/**
 * Two values are the same wire value when they serialise to the same JSON,
 * whatever order their properties were built in: a materialised item and the
 * editor's copy of it must compare equal, or every save re-walks the document.
 */
function areWireValuesEqual(left: unknown, right: unknown): boolean {
  return areJsonValuesEqual(left, right);
}

function isRecord(value: unknown): value is ClientRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
