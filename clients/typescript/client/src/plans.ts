/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Plans generated from a Clerkenwell definition: the TypeScript counterpart of
 * `clerkenwell-schema`. Name parameters default to `string`; an application
 * instantiates them with its generated name unions.
 */

export type CollaborationStorageKind =
  | "scalar"
  | "text"
  | "orderedList"
  | "structuredList"
  | "structuredMap"
  | "structuredDocument"
  | "derivedIdentity"
  | "derivedRevision"
  | "keyedSequence";

export type CollaborationValueCodec =
  | "integer"
  | "number"
  | "optionalNumber"
  | "boolean"
  | "string"
  | "optionalString"
  | "propertyText"
  | "stringList"
  | "structuredJson"
  | "identity"
  | "keyedSequence";

export type CollaborationConflictPolicy =
  | "immutable"
  | "explicit"
  | "merge"
  | "lastWriterWins";

/**
 * Where a field lives in the document. `container` names one container;
 * `containerTemplate` names one per keyed item through `{identity}`
 * placeholders that {@link resolveCollaborationContainer} fills.
 */
export interface CollaborationFieldStorage {
  readonly kind: CollaborationStorageKind;
  readonly container?: string;
  readonly containerTemplate?: string;
  readonly key?: string;
  readonly identityPath?: string;
  readonly identityVariable?: string;
  readonly orderContainer?: string;
  readonly itemContainerTemplate?: string;
  readonly metadataContainer?: string;
  readonly metadataContainerTemplate?: string;
  readonly metadataKey?: string;
}

export interface CollaborationFieldPlan {
  /** Dotted wire path; `*` stands for one item of the enclosing keyed sequence. */
  readonly path: string;
  readonly storage: CollaborationFieldStorage;
  readonly value: {
    readonly codec: CollaborationValueCodec;
    readonly schema?: { readonly $ref: string };
  };
  readonly required: boolean;
  readonly conflict: CollaborationConflictPolicy;
}

export interface CollaborationEntityPlan<
  TProjection extends string = string,
  TMutation extends string = string
> {
  readonly substrate: "loro";
  readonly schemaVersion: number;
  readonly migrationIds: readonly string[];
  readonly authoringState: {
    readonly projection: TProjection;
    readonly importMutation: TMutation;
  };
  readonly rootContainer: string;
  /** Wire paths are snake_case; client documents carry them as camelCase. */
  readonly clientPathNaming: "camelCase";
  readonly fields: Readonly<Record<string, CollaborationFieldPlan>>;
}

/** The paths of a plan's declared text fields. */
export type CollaborationPlanTextFieldPath<TPlan extends CollaborationEntityPlan> = {
  [TPath in keyof TPlan["fields"] & string]: TPlan["fields"][TPath] extends {
    readonly storage: { readonly kind: "text" };
  }
    ? TPath
    : never;
}[keyof TPlan["fields"] & string];

export function resolveCollaborationContainer(
  template: string,
  identities: Readonly<Record<string, string>>
): string {
  return template.replace(/\{([^{}]*)\}|\{/g, (_match, identity: string | undefined) => {
    if (identity === undefined) {
      throw new Error(`Unclosed placeholder in collaboration container ${JSON.stringify(template)}.`);
    }
    const value = identities[identity];
    if (value === undefined || value.length === 0) {
      throw new Error(
        `Missing collaboration identity ${JSON.stringify(identity)} for ${JSON.stringify(template)}.`
      );
    }
    return value;
  });
}

/** How a client folds one projection's patches into its last snapshot. */
export type ProjectionCompositionPlan<TEntity extends string = string> =
  | {
    readonly strategy: "keyedCollection";
    readonly collectionField: string;
    readonly itemField: string;
    readonly itemIdentityField: string;
    readonly patchIdentityField: string;
    readonly dependsOn: readonly TEntity[];
  }
  | {
    readonly strategy: "sequencedText";
    readonly collectionField: string;
    readonly itemIdentityField: string;
    readonly patchIdentityField: string;
    readonly snapshotOutputField: string;
    readonly sequenceField: string;
    readonly textField: string;
    readonly patchOutputField: string;
    readonly deltaTextField: string;
    readonly dependsOn: readonly TEntity[];
  }
  | {
    readonly strategy: "replaceOrRemove";
    readonly updateField?: string;
    readonly updatesField?: string;
    readonly snapshotMode: "patch" | "field";
    readonly snapshotField?: string;
    readonly snapshotOmitFields?: readonly string[];
    readonly removeMode: "nullSnapshot" | "nullField";
    readonly removeField?: string;
    readonly dependsOn: readonly TEntity[];
  }
  | {
    readonly strategy: "replace";
    readonly snapshotMode: "patch" | "field";
    readonly snapshotField?: string;
    readonly snapshotOmitFields?: readonly string[];
    readonly dependsOn: readonly TEntity[];
  }
  | {
    readonly strategy: "reset";
    readonly dependsOn: readonly TEntity[];
  };

export type ProjectionCompositionPlans<
  TProjection extends string = string,
  TEntity extends string = string
> = { readonly [K in TProjection]: ProjectionCompositionPlan<TEntity> };

export type AuthoringPolicyKind =
  | "optimisticDocument"
  | "collaborative"
  | "commandOwned"
  | "readOnly";

export interface AuthoringPlan<TMutation extends string = string> {
  readonly kind: AuthoringPolicyKind;
  readonly schemaVersion: number;
  readonly rationale: string;
  readonly revision:
    | { readonly kind: "contentHash"; readonly field: string }
    | { readonly kind: "loro" };
  readonly mutations: readonly TMutation[];
  readonly contentMutation?: TMutation;
  readonly planningMutations: readonly TMutation[];
  readonly commandMutations: readonly TMutation[];
  readonly lifecycleMutations: readonly TMutation[];
  readonly authoringSession: null | {
    /** The storage key the application persists the authoring runtime under. */
    readonly mnemonicKey: string;
    readonly leavePolicy: "durableRestoreOrConfirmDiscard";
    readonly conflictPolicy: "generatedFieldPolicy" | "expectedRevision";
  };
}
