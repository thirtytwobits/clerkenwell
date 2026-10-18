/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The names and payload types of an application's projections, mutations and
 * commands.
 * A definition generates one model; everything in this package that touches a
 * payload is generic over it.
 */

export interface ProjectionContract {
  readonly params: unknown;
  readonly snapshot: unknown;
  readonly patch: unknown;
}

export interface MutationContract {
  readonly params: unknown;
  readonly result: unknown;
}

export interface CommandContract {
  readonly params: unknown;
  readonly result: unknown;
}

export interface ProjectionModel {
  readonly projections: { readonly [projection: string]: ProjectionContract };
  readonly mutations: { readonly [mutation: string]: MutationContract };
  readonly commands: { readonly [command: string]: CommandContract };
}

export type ProjectionName<M extends ProjectionModel = ProjectionModel> =
  keyof M["projections"] & string;

export type MutationName<M extends ProjectionModel = ProjectionModel> =
  keyof M["mutations"] & string;

export type CommandName<M extends ProjectionModel = ProjectionModel> =
  keyof M["commands"] & string;

export type ProjectionParams<
  M extends ProjectionModel,
  K extends ProjectionName<M>
> = M["projections"][K]["params"];

export type ProjectionSnapshot<
  M extends ProjectionModel,
  K extends ProjectionName<M>
> = M["projections"][K]["snapshot"];

export type ProjectionPatch<
  M extends ProjectionModel,
  K extends ProjectionName<M>
> = M["projections"][K]["patch"];

export type MutationParams<
  M extends ProjectionModel,
  K extends MutationName<M>
> = M["mutations"][K]["params"];

export type MutationResult<
  M extends ProjectionModel,
  K extends MutationName<M>
> = M["mutations"][K]["result"];

export type CommandParams<
  M extends ProjectionModel,
  K extends CommandName<M>
> = M["commands"][K]["params"];

export type CommandResult<
  M extends ProjectionModel,
  K extends CommandName<M>
> = M["commands"][K]["result"];

export type ProjectionTransportSnapshot<M extends ProjectionModel = ProjectionModel> = {
  [K in ProjectionName<M>]: { projection: K; value: ProjectionSnapshot<M, K> };
}[ProjectionName<M>];

export type ProjectionTransportPatch<M extends ProjectionModel = ProjectionModel> = {
  [K in ProjectionName<M>]: { projection: K; value: ProjectionPatch<M, K> };
}[ProjectionName<M>];

export type ProjectionTransportMutationResult<M extends ProjectionModel = ProjectionModel> = {
  [K in MutationName<M>]: { mutation: K; value: MutationResult<M, K> };
}[MutationName<M>];

export type ProjectionTransportCommandResult<M extends ProjectionModel = ProjectionModel> = {
  [K in CommandName<M>]: { command: K; value: CommandResult<M, K> };
}[CommandName<M>];
