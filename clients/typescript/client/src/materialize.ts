/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Definition-driven projection snapshot and patch materialisation.
 */
import type {
  ProjectionCompositionPlan,
  ProjectionCompositionPlans
} from "./plans";
import type {
  ProjectionModel,
  ProjectionName,
  ProjectionSnapshot,
  ProjectionTransportPatch
} from "./projection-model";
import type { ProjectionTransportEvent } from "./protocol";

export interface MaterializedProjectionValue<TSnapshot> {
  subscription_id: number;
  revision: number;
  snapshot: TSnapshot | null;
}

export type MaterializedProjectionFor<
  M extends ProjectionModel,
  K extends ProjectionName<M>
> = {
  projection: K;
  value: MaterializedProjectionValue<ProjectionSnapshot<M, K>>;
};

export type MaterializedProjection<M extends ProjectionModel = ProjectionModel> = {
  [K in ProjectionName<M>]: MaterializedProjectionFor<M, K>;
}[ProjectionName<M>];

export interface ProjectionMaterializerState<M extends ProjectionModel = ProjectionModel> {
  readonly plans: ProjectionCompositionPlans<ProjectionName<M>>;
  subscriptions: Map<number, MaterializedProjection<M>>;
}

type JsonRecord = Record<string, unknown>;

export function createProjectionMaterializerState<M extends ProjectionModel = ProjectionModel>(
  plans: ProjectionCompositionPlans<ProjectionName<M>>
): ProjectionMaterializerState<M> {
  return {
    plans,
    subscriptions: new Map()
  };
}

export function applyProjectionEvent<M extends ProjectionModel>(
  state: ProjectionMaterializerState<M>,
  event: ProjectionTransportEvent<M>
): MaterializedProjection<M> {
  switch (event.kind) {
    case "snapshot":
      return applyProjectionSnapshot(state, event);
    case "patch":
      return applyProjectionPatch(state, event);
  }
}

function applyProjectionSnapshot<M extends ProjectionModel>(
  state: ProjectionMaterializerState<M>,
  event: Extract<ProjectionTransportEvent<M>, { kind: "snapshot" }>
): MaterializedProjection<M> {
  const materialized = {
    projection: event.snapshot.projection,
    value: {
      subscription_id: event.subscription_id,
      revision: event.revision,
      snapshot: event.snapshot.value
    }
  } as MaterializedProjection<M>;
  state.subscriptions.set(event.subscription_id, materialized);
  return materialized;
}

function applyProjectionPatch<M extends ProjectionModel>(
  state: ProjectionMaterializerState<M>,
  event: Extract<ProjectionTransportEvent<M>, { kind: "patch" }>
): MaterializedProjection<M> {
  const materialized = state.subscriptions.get(event.subscription_id);
  if (!materialized) {
    throw new ProjectionMaterializerError(
      `Received patch for unknown subscription ${event.subscription_id}.`
    );
  }
  if (materialized.projection !== event.patch.projection) {
    throw new ProjectionMaterializerError(
      `Received ${event.patch.projection} patch for ${materialized.projection} subscription ${event.subscription_id}.`
    );
  }
  if (materialized.value.revision !== event.from_revision) {
    throw new ProjectionMaterializerError(
      `Received patch from revision ${event.from_revision} for subscription ${event.subscription_id} at revision ${materialized.value.revision}.`
    );
  }
  if (event.to_revision <= event.from_revision) {
    throw new ProjectionMaterializerError(
      `Received non-advancing patch revision ${event.from_revision} -> ${event.to_revision} for subscription ${event.subscription_id}.`
    );
  }
  const plan: ProjectionCompositionPlan | undefined = state.plans[event.patch.projection];
  if (plan === undefined) {
    throw new ProjectionMaterializerError(
      `No composition plan exists for projection ${event.patch.projection}.`
    );
  }

  materialized.value.snapshot = materializePatch(
    materialized.value.snapshot,
    event.patch,
    plan
  ) as typeof materialized.value.snapshot;
  materialized.value.revision = event.to_revision;
  return materialized;
}

function materializePatch(
  currentSnapshot: unknown,
  patch: ProjectionTransportPatch,
  plan: ProjectionCompositionPlan
): unknown {
  const patchValue = requireRecord(patch.value, `${patch.projection} patch`);
  const kind = requireString(patchValue.kind, `${patch.projection} patch kind`);

  switch (plan.strategy) {
    case "keyedCollection":
      return materializeKeyedCollection(currentSnapshot, patchValue, kind, plan);
    case "sequencedText":
      return materializeSequencedText(currentSnapshot, patchValue, kind, plan);
    case "replaceOrRemove":
      return materializeReplaceOrRemove(currentSnapshot, patchValue, kind, plan);
    case "replace":
      if (kind !== "replace") {
        throw unexpectedPatchKind(patch.projection, kind, plan.strategy);
      }
      return snapshotFromPatch(patchValue, plan);
    case "reset":
      if (kind !== "reset") {
        throw unexpectedPatchKind(patch.projection, kind, plan.strategy);
      }
      return omitFields(patchValue, ["kind"]);
  }
}

function materializeSequencedText(
  currentSnapshot: unknown,
  patch: JsonRecord,
  kind: string,
  plan: Extract<ProjectionCompositionPlan, { strategy: "sequencedText" }>
): JsonRecord {
  if (kind === "reset") {
    return omitFields(patch, ["kind"]);
  }
  if (kind !== "output") {
    throw unexpectedPatchKind("sequenced text", kind, plan.strategy);
  }

  const current = requireRecord(currentSnapshot, "sequenced text snapshot");
  const collection = requireArray(
    current[plan.collectionField],
    `${plan.collectionField} snapshot field`
  );
  const identity = patch[plan.patchIdentityField];
  if (identity === undefined) {
    throw new ProjectionMaterializerError(
      `Output patch is missing identity field ${plan.patchIdentityField}.`
    );
  }
  const delta = requireRecord(
    patch[plan.patchOutputField],
    `${plan.patchOutputField} output field`
  );
  const sequence = requireInteger(
    delta[plan.sequenceField],
    `${plan.sequenceField} output sequence`
  );
  const textDelta = requireString(
    delta[plan.deltaTextField],
    `${plan.deltaTextField} output text`
  );
  let matched = false;
  const next = collection.map((candidate) => {
    if (!isRecord(candidate) || candidate[plan.itemIdentityField] !== identity) {
      return candidate;
    }
    matched = true;
    const currentOutput = candidate[plan.snapshotOutputField] === undefined
      ? null
      : requireRecord(
          candidate[plan.snapshotOutputField],
          `${plan.snapshotOutputField} snapshot field`
        );
    const currentSequence = currentOutput === null
      ? 0
      : requireInteger(
          currentOutput[plan.sequenceField],
          `${plan.sequenceField} snapshot sequence`
        );
    if (sequence !== currentSequence + 1) {
      throw new ProjectionMaterializerError(
        `Received output sequence ${sequence} after ${currentSequence} for ${String(identity)}.`
      );
    }
    const currentText = currentOutput === null
      ? ""
      : requireString(
          currentOutput[plan.textField],
          `${plan.textField} snapshot text`
        );
    return {
      ...candidate,
      [plan.snapshotOutputField]: {
        [plan.sequenceField]: sequence,
        [plan.textField]: `${currentText}${textDelta}`
      }
    };
  });
  if (!matched) {
    throw new ProjectionMaterializerError(
      `Output patch targets unknown identity ${String(identity)}.`
    );
  }
  return { ...current, [plan.collectionField]: next };
}

function materializeKeyedCollection(
  currentSnapshot: unknown,
  patch: JsonRecord,
  kind: string,
  plan: Extract<ProjectionCompositionPlan, { strategy: "keyedCollection" }>
): JsonRecord {
  const current = currentSnapshot === null
    ? {}
    : requireRecord(currentSnapshot, "keyed collection snapshot");
  const existing = current[plan.collectionField];
  const collection = existing === undefined
    ? []
    : requireArray(existing, `${plan.collectionField} snapshot field`);

  switch (kind) {
    case "reset":
      return {
        ...current,
        [plan.collectionField]: requireArray(
          patch[plan.collectionField],
          `${plan.collectionField} reset field`
        )
      };
    case "upsert": {
      const item = requireRecord(
        patch[plan.itemField],
        `${plan.itemField} upsert field`
      );
      const identity = item[plan.itemIdentityField];
      if (identity === undefined) {
        throw new ProjectionMaterializerError(
          `Upsert item is missing identity field ${plan.itemIdentityField}.`
        );
      }
      const next = collection.filter((candidate) => (
        !isRecord(candidate)
        || candidate[plan.itemIdentityField] !== identity
      ));
      next.push(item);
      return { ...current, [plan.collectionField]: next };
    }
    case "remove": {
      const identity = patch[plan.patchIdentityField];
      if (identity === undefined) {
        throw new ProjectionMaterializerError(
          `Remove patch is missing identity field ${plan.patchIdentityField}.`
        );
      }
      return {
        ...current,
        [plan.collectionField]: collection.filter((candidate) => (
          !isRecord(candidate)
          || candidate[plan.itemIdentityField] !== identity
        ))
      };
    }
    default:
      throw unexpectedPatchKind("keyed collection", kind, plan.strategy);
  }
}

function materializeReplaceOrRemove(
  currentSnapshot: unknown,
  patch: JsonRecord,
  kind: string,
  plan: Extract<ProjectionCompositionPlan, { strategy: "replaceOrRemove" }>
): unknown {
  switch (kind) {
    case "update": {
      const field = requirePlanField(plan.updateField, "updateField");
      const changes = requireRecord(patch[requirePlanField(plan.updatesField, "updatesField")], "field updates");
      const snapshot = requireRecord(currentSnapshot, "field update snapshot");
      const target = requireRecord(snapshot[field], "field update target");
      return { ...snapshot, [field]: { ...target, ...changes } };
    }
    case "replace":
      return snapshotFromPatch(patch, plan);
    case "remove":
      if (plan.removeMode === "nullSnapshot") {
        return null;
      }
      return {
        ...(currentSnapshot === null
          ? {}
          : requireRecord(currentSnapshot, "replace-or-remove snapshot")),
        ...omitFields(
          patch,
          ["kind", ...(plan.snapshotOmitFields ?? [])]
        ),
        [requirePlanField(plan.removeField, "removeField")]: null
      };
    default:
      throw unexpectedPatchKind("replace-or-remove", kind, plan.strategy);
  }
}

function snapshotFromPatch(
  patch: JsonRecord,
  plan: Extract<
    ProjectionCompositionPlan,
    { strategy: "replace" | "replaceOrRemove" }
  >
): unknown {
  if (plan.snapshotMode === "field") {
    const field = requirePlanField(plan.snapshotField, "snapshotField");
    if (!(field in patch)) {
      throw new ProjectionMaterializerError(
        `Patch is missing generated snapshot field ${field}.`
      );
    }
    return patch[field];
  }
  return omitFields(patch, ["kind", ...(plan.snapshotOmitFields ?? [])]);
}

function omitFields(record: JsonRecord, fields: readonly string[]): JsonRecord {
  const omitted = new Set(fields);
  return Object.fromEntries(
    Object.entries(record).filter(([field]) => !omitted.has(field))
  );
}

function requirePlanField(
  field: string | undefined,
  name: string
): string {
  if (field === undefined) {
    throw new ProjectionMaterializerError(
      `Generated composition plan is missing ${name}.`
    );
  }
  return field;
}

function requireRecord(value: unknown, context: string): JsonRecord {
  if (!isRecord(value)) {
    throw new ProjectionMaterializerError(`${context} must be an object.`);
  }
  return value;
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function requireArray(value: unknown, context: string): unknown[] {
  if (!Array.isArray(value)) {
    throw new ProjectionMaterializerError(`${context} must be an array.`);
  }
  return value;
}

function requireString(value: unknown, context: string): string {
  if (typeof value !== "string") {
    throw new ProjectionMaterializerError(`${context} must be a string.`);
  }
  return value;
}

function requireInteger(value: unknown, context: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    throw new ProjectionMaterializerError(`${context} must be an integer.`);
  }
  return value;
}

function unexpectedPatchKind(
  projection: string,
  kind: string,
  strategy: ProjectionCompositionPlan["strategy"]
): ProjectionMaterializerError {
  return new ProjectionMaterializerError(
    `Received ${kind} patch for ${projection} using ${strategy} materialisation.`
  );
}

export class ProjectionMaterializerError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProjectionMaterializerError";
  }
}
