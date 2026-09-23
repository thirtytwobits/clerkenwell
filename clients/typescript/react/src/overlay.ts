/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Reusable local edit overlay state for projection-backed React surfaces.
 */
import { ProjectionExternalStore } from "./store";
import type { ProjectionReadableStore, ProjectionReactStoreListener } from "./store";

export type ProjectionOverlayStatus =
  | "clean"
  | "dirty"
  | "stale"
  | "committing"
  | "conflicted";

export interface ProjectionOverlayConflict<TField extends string> {
  kind: "stale" | "commit";
  fields: ReadonlySet<TField>;
  message: string;
}

export interface ProjectionEditOverlaySnapshot<
  TDraft extends object,
  TRevision,
  TField extends keyof TDraft & string = keyof TDraft & string
> {
  authoritative: Readonly<TDraft> | null;
  draft: Readonly<TDraft>;
  dirtyFields: ReadonlySet<TField>;
  committingFields: ReadonlySet<TField>;
  revision: TRevision | null;
  status: ProjectionOverlayStatus;
  conflict: ProjectionOverlayConflict<TField> | null;
}

export interface ProjectionEditOverlayOptions<TDraft extends object> {
  emptyDraft: TDraft;
  cloneDraft?: (draft: Readonly<TDraft>) => TDraft;
}

export class ProjectionEditOverlay<
  TDraft extends object,
  TRevision = number,
  TField extends keyof TDraft & string = keyof TDraft & string
> implements ProjectionReadableStore<ProjectionEditOverlaySnapshot<TDraft, TRevision, TField>> {
  private readonly store: ProjectionExternalStore<
    ProjectionEditOverlaySnapshot<TDraft, TRevision, TField>
  >;
  private readonly cloneDraft: (draft: Readonly<TDraft>) => TDraft;

  constructor(options: ProjectionEditOverlayOptions<TDraft>) {
    this.cloneDraft = options.cloneDraft ?? shallowCloneDraft;
    const emptyDraft = this.cloneDraft(options.emptyDraft);
    this.store = new ProjectionExternalStore({
      authoritative: null,
      draft: this.cloneDraft(emptyDraft),
      dirtyFields: new Set<TField>(),
      committingFields: new Set<TField>(),
      revision: null,
      status: "clean",
      conflict: null
    });
  }

  subscribe = (listener: ProjectionReactStoreListener): (() => void) => (
    this.store.subscribe(listener)
  );

  getSnapshot = (): ProjectionEditOverlaySnapshot<TDraft, TRevision, TField> => (
    this.store.getSnapshot()
  );

  hydrate(authoritative: TDraft, revision: TRevision | null): void {
    const snapshot = this.getSnapshot();
    const draft = this.cloneDraft(snapshot.draft);
    const dirtyFields = cloneSet(snapshot.dirtyFields);
    const staleFields = new Set<TField>();

    typedKeys(authoritative).forEach((field) => {
      if (dirtyFields.has(field as TField)) {
        if (Object.is(draft[field], authoritative[field])) {
          dirtyFields.delete(field as TField);
        } else if (
          snapshot.authoritative
          && !Object.is(snapshot.authoritative[field], authoritative[field])
        ) {
          staleFields.add(field as TField);
        }
      } else {
        draft[field] = authoritative[field];
      }
    });

    const conflict = staleFields.size > 0
      ? staleConflict(staleFields)
      : matchingConflict(snapshot.conflict, dirtyFields);

    this.store.setSnapshot({
      authoritative: this.cloneDraft(authoritative),
      draft,
      dirtyFields,
      committingFields: cloneSet(snapshot.committingFields),
      revision,
      status: overlayStatus(dirtyFields, snapshot.committingFields, conflict),
      conflict
    });
  }

  restoreDraft(
    draft: TDraft,
    options?: {
      authoritative?: TDraft | null;
      dirtyFields?: Iterable<TField>;
      revision?: TRevision | null;
    }
  ): void {
    const dirtyFields = new Set(options?.dirtyFields ?? []);
    this.store.setSnapshot({
      authoritative: options?.authoritative ? this.cloneDraft(options.authoritative) : null,
      draft: this.cloneDraft(draft),
      dirtyFields,
      committingFields: new Set<TField>(),
      revision: options?.revision ?? null,
      status: overlayStatus(dirtyFields, new Set<TField>(), null),
      conflict: null
    });
  }

  setField<TFieldName extends TField>(
    field: TFieldName,
    value: TDraft[TFieldName]
  ): void {
    const snapshot = this.getSnapshot();
    const draft = this.cloneDraft(snapshot.draft);
    draft[field] = value;

    const dirtyFields = cloneSet(snapshot.dirtyFields);
    if (snapshot.authoritative && Object.is(snapshot.authoritative[field], value)) {
      dirtyFields.delete(field);
    } else {
      dirtyFields.add(field);
    }

    this.store.setSnapshot({
      ...snapshot,
      draft,
      dirtyFields,
      status: overlayStatus(dirtyFields, snapshot.committingFields, null),
      conflict: null
    });
  }

  keepDraft(fields?: Iterable<TField>): void {
    const snapshot = this.getSnapshot();
    const conflict = removeConflictFields(snapshot.conflict, fields);
    this.store.setSnapshot({
      ...snapshot,
      status: overlayStatus(snapshot.dirtyFields, snapshot.committingFields, conflict),
      conflict
    });
  }

  resetFields(fields: Iterable<TField>): void {
    const snapshot = this.getSnapshot();
    const authoritative = snapshot.authoritative;
    if (!authoritative) {
      return;
    }

    const draft = this.cloneDraft(snapshot.draft);
    const dirtyFields = cloneSet(snapshot.dirtyFields);
    const committingFields = cloneSet(snapshot.committingFields);

    Array.from(fields).forEach((field) => {
      draft[field] = authoritative[field];
      dirtyFields.delete(field);
      committingFields.delete(field);
    });

    const conflict = removeConflictFields(snapshot.conflict, fields);
    this.store.setSnapshot({
      ...snapshot,
      draft,
      dirtyFields,
      committingFields,
      status: overlayStatus(dirtyFields, committingFields, conflict),
      conflict
    });
  }

  markCommitting(fields: Iterable<TField>): void {
    const snapshot = this.getSnapshot();
    const committingFields = cloneSet(snapshot.committingFields);
    Array.from(fields).forEach((field) => committingFields.add(field));

    this.store.setSnapshot({
      ...snapshot,
      committingFields,
      status: overlayStatus(snapshot.dirtyFields, committingFields, null),
      conflict: null
    });
  }

  markCommitted(
    fields: Iterable<TField>,
    authoritative: TDraft,
    revision: TRevision | null
  ): void {
    const committedFields = new Set(fields);
    const snapshot = this.getSnapshot();
    const dirtyFields = cloneSet(snapshot.dirtyFields);
    const committingFields = cloneSet(snapshot.committingFields);

    committedFields.forEach((field) => {
      dirtyFields.delete(field);
      committingFields.delete(field);
    });

    const draft = this.cloneDraft(snapshot.draft);
    typedKeys(authoritative).forEach((field) => {
      if (!dirtyFields.has(field as TField)) {
        draft[field] = authoritative[field];
      }
    });

    this.store.setSnapshot({
      authoritative: this.cloneDraft(authoritative),
      draft,
      dirtyFields,
      committingFields,
      revision,
      status: overlayStatus(dirtyFields, committingFields, null),
      conflict: null
    });
  }

  markCommitFailed(fields: Iterable<TField>, error: unknown): void {
    const snapshot = this.getSnapshot();
    const committingFields = cloneSet(snapshot.committingFields);
    const failedFields = new Set(fields);
    failedFields.forEach((field) => committingFields.delete(field));
    const conflict = {
      kind: "commit" as const,
      fields: failedFields,
      message: errorMessage(error)
    };

    this.store.setSnapshot({
      ...snapshot,
      committingFields,
      status: "conflicted",
      conflict
    });
  }

  reset(authoritative?: TDraft, revision?: TRevision | null): void {
    const nextAuthoritative = authoritative
      ?? this.getSnapshot().authoritative
      ?? this.getSnapshot().draft;
    this.store.setSnapshot({
      authoritative: this.cloneDraft(nextAuthoritative),
      draft: this.cloneDraft(nextAuthoritative),
      dirtyFields: new Set<TField>(),
      committingFields: new Set<TField>(),
      revision: revision ?? this.getSnapshot().revision,
      status: "clean",
      conflict: null
    });
  }

  dispose(): void {
    this.store.dispose();
  }
}

function shallowCloneDraft<TDraft extends object>(
  draft: Readonly<TDraft>
): TDraft {
  return { ...draft } as TDraft;
}

function typedKeys<TDraft extends object>(
  draft: TDraft
): Array<keyof TDraft & string> {
  return Object.keys(draft) as Array<keyof TDraft & string>;
}

function cloneSet<TValue>(values: ReadonlySet<TValue>): Set<TValue> {
  return new Set(values);
}

function overlayStatus<TField extends string>(
  dirtyFields: ReadonlySet<TField>,
  committingFields: ReadonlySet<TField>,
  conflict: ProjectionOverlayConflict<TField> | null
): ProjectionOverlayStatus {
  if (conflict) {
    return conflict.kind === "stale" ? "stale" : "conflicted";
  }
  if (committingFields.size > 0) {
    return "committing";
  }
  return dirtyFields.size > 0 ? "dirty" : "clean";
}

function staleConflict<TField extends string>(
  fields: ReadonlySet<TField>
): ProjectionOverlayConflict<TField> {
  const fieldList = [...fields].join(", ");
  return {
    kind: "stale",
    fields,
    message: `Remote changes touched ${fieldList} while local edits are pending.`
  };
}

function matchingConflict<TField extends string>(
  conflict: ProjectionOverlayConflict<TField> | null,
  dirtyFields: ReadonlySet<TField>
): ProjectionOverlayConflict<TField> | null {
  if (!conflict) {
    return null;
  }
  const fields = new Set([...conflict.fields].filter((field) => dirtyFields.has(field)));
  if (fields.size === 0) {
    return null;
  }
  return {
    ...conflict,
    fields
  };
}

function removeConflictFields<TField extends string>(
  conflict: ProjectionOverlayConflict<TField> | null,
  fields?: Iterable<TField>
): ProjectionOverlayConflict<TField> | null {
  if (!conflict) {
    return null;
  }
  if (!fields) {
    return null;
  }
  const remaining = cloneSet(conflict.fields);
  Array.from(fields).forEach((field) => remaining.delete(field));
  if (remaining.size === 0) {
    return null;
  }
  return {
    ...conflict,
    fields: remaining
  };
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}
