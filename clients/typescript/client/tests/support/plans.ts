/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Hand-written plans for a note and a task board, shaped as a generator emits
 * them, with the client documents they describe.
 */
import type { CollaborationEntityPlan } from "@clerkenwell/client";

/** One field of every storage kind and scalar codec a root-level document can hold. */
export const NOTE_PLAN = {
  substrate: "loro",
  schemaVersion: 2,
  migrationIds: ["note-v1", "note-v2"],
  authoringState: { projection: "notes.authoringState", importMutation: "note.importUpdate" },
  rootContainer: "note",
  clientPathNaming: "camelCase",
  fields: {
    note_id: {
      path: "note_id",
      storage: { kind: "scalar", container: "note", key: "note_id" },
      value: { codec: "string" },
      required: true,
      conflict: "immutable"
    },
    title: {
      path: "title",
      storage: { kind: "scalar", container: "note", key: "title" },
      value: { codec: "string" },
      required: true,
      conflict: "explicit"
    },
    pinned: {
      path: "pinned",
      storage: { kind: "scalar", container: "note", key: "pinned" },
      value: { codec: "boolean" },
      required: true,
      conflict: "explicit"
    },
    priority: {
      path: "priority",
      storage: { kind: "scalar", container: "note", key: "priority" },
      value: { codec: "integer" },
      required: true,
      conflict: "explicit"
    },
    weight: {
      path: "weight",
      storage: { kind: "scalar", container: "note", key: "weight" },
      value: { codec: "number" },
      required: true,
      conflict: "lastWriterWins"
    },
    rating: {
      path: "rating",
      storage: { kind: "scalar", container: "note", key: "rating" },
      value: { codec: "optionalNumber" },
      required: false,
      conflict: "explicit"
    },
    subtitle: {
      path: "subtitle",
      storage: { kind: "scalar", container: "note", key: "subtitle" },
      value: { codec: "optionalString" },
      required: false,
      conflict: "explicit"
    },
    "style.font_family": {
      path: "style.font_family",
      storage: { kind: "scalar", container: "note", key: "style.font_family" },
      value: { codec: "string" },
      required: true,
      conflict: "explicit"
    },
    "style.accent_colour": {
      path: "style.accent_colour",
      storage: { kind: "scalar", container: "note", key: "style.accent_colour" },
      value: { codec: "optionalString" },
      required: false,
      conflict: "explicit"
    },
    body: {
      path: "body",
      storage: { kind: "text", container: "body" },
      value: { codec: "string" },
      required: true,
      conflict: "merge"
    },
    summary: {
      path: "summary",
      storage: { kind: "text", container: "summary" },
      value: { codec: "optionalString" },
      required: false,
      conflict: "merge"
    },
    abstract: {
      path: "abstract",
      storage: {
        kind: "text",
        container: "abstract",
        metadataContainer: "note",
        metadataKey: "abstract.$mime"
      },
      value: { codec: "propertyText" },
      required: true,
      conflict: "merge"
    },
    tags: {
      path: "tags",
      storage: { kind: "orderedList", container: "tags" },
      value: { codec: "stringList" },
      required: true,
      conflict: "merge"
    },
    links: {
      path: "links",
      storage: { kind: "structuredList", container: "links" },
      value: { codec: "structuredJson", schema: { $ref: "#/$defs/NoteLink" } },
      required: true,
      conflict: "merge"
    },
    attributes: {
      path: "attributes",
      storage: { kind: "structuredMap", container: "attributes" },
      value: { codec: "structuredJson" },
      required: false,
      conflict: "merge"
    },
    outline: {
      path: "outline",
      storage: { kind: "structuredDocument", container: "outline" },
      value: { codec: "structuredJson" },
      required: false,
      conflict: "merge"
    },
    author: {
      path: "author",
      storage: { kind: "scalar", container: "note", key: "author" },
      value: { codec: "string" },
      required: false,
      conflict: "explicit"
    },
    footnote: {
      path: "footnote",
      storage: { kind: "text", container: "footnote" },
      value: { codec: "string" },
      required: false,
      conflict: "merge"
    },
    caption: {
      path: "caption",
      storage: {
        kind: "text",
        container: "caption",
        metadataContainer: "note",
        metadataKey: "caption.$mime"
      },
      value: { codec: "propertyText" },
      required: false,
      conflict: "merge"
    },
    aliases: {
      path: "aliases",
      storage: { kind: "orderedList", container: "aliases" },
      value: { codec: "stringList" },
      required: false,
      conflict: "merge"
    },
    references: {
      path: "references",
      storage: { kind: "structuredList", container: "references" },
      value: { codec: "structuredJson", schema: { $ref: "#/$defs/NoteLink" } },
      required: false,
      conflict: "merge"
    },
    etag: {
      path: "etag",
      storage: { kind: "derivedRevision" },
      value: { codec: "string" },
      required: true,
      conflict: "immutable"
    }
  }
} as const satisfies CollaborationEntityPlan<"notes.authoringState", "note.importUpdate">;

export interface NoteLink {
  targetId: string;
  linkLabel: string;
}

export interface NoteDocument {
  noteId: string;
  title: string;
  pinned: boolean;
  priority: number;
  weight: number;
  rating?: number;
  subtitle?: string;
  style: { fontFamily: string; accentColour?: string };
  body: string;
  summary?: string;
  abstract: { "$mime": string; value: string };
  tags: string[];
  links: NoteLink[];
  attributes?: Record<string, unknown>;
  outline?: Record<string, unknown>;
  author?: string;
  footnote?: string;
  caption?: { "$mime": string; value: string };
  aliases?: string[];
  references?: NoteLink[];
  etag?: string;
}

export function noteDocument(): NoteDocument {
  return {
    noteId: "note-1",
    title: "Minutes",
    pinned: true,
    priority: 3,
    weight: 0.75,
    rating: 4.5,
    subtitle: "Weekly",
    style: { fontFamily: "Serif", accentColour: "teal" },
    body: "Agreed to move the stand-up. 🦊 Everyone nodded.",
    summary: "Stand-up moves.",
    abstract: { "$mime": "text/markdown", value: "# Minutes\nShort." },
    tags: ["meeting", "weekly"],
    links: [{ targetId: "note-0", linkLabel: "Previous" }],
    attributes: { owner: "iris", reviewed: false, counts: { open: 2 } },
    outline: {
      heading: "Agenda",
      items: [{ id: "item-1", text: "Stand-up" }, { id: "item-2", text: "Retro" }]
    },
    author: "Iris",
    footnote: "Minuted by Iris.",
    caption: { "$mime": "text/plain", value: "Friday's minutes" },
    aliases: ["stand-up notes"],
    references: [{ targetId: "note-9", linkLabel: "Background" }]
  };
}

/**
 * Nested keyed sequences: columns keyed by `column_id`, and inside each a
 * task sequence keyed by `id` under its own identity variable.
 */
export const BOARD_PLAN = {
  substrate: "loro",
  schemaVersion: 1,
  migrationIds: ["board-v1"],
  authoringState: { projection: "boards.authoringState", importMutation: "board.importUpdate" },
  rootContainer: "board",
  clientPathNaming: "camelCase",
  fields: {
    board_id: {
      path: "board_id",
      storage: { kind: "scalar", container: "board", key: "board_id" },
      value: { codec: "string" },
      required: true,
      conflict: "immutable"
    },
    title: {
      path: "title",
      storage: { kind: "scalar", container: "board", key: "title" },
      value: { codec: "string" },
      required: true,
      conflict: "explicit"
    },
    description: {
      path: "description",
      storage: { kind: "text", container: "board.description" },
      value: { codec: "optionalString" },
      required: false,
      conflict: "merge"
    },
    columns: {
      path: "columns",
      storage: {
        kind: "keyedSequence",
        identityPath: "column_id",
        orderContainer: "board.column_order",
        itemContainerTemplate: "board.column.{column_id}"
      },
      value: { codec: "keyedSequence" },
      required: true,
      conflict: "merge"
    },
    "columns.*.column_id": {
      path: "columns.*.column_id",
      storage: { kind: "derivedIdentity", identityPath: "column_id" },
      value: { codec: "identity" },
      required: true,
      conflict: "immutable"
    },
    "columns.*.name": {
      path: "columns.*.name",
      storage: { kind: "scalar", containerTemplate: "board.column.{column_id}", key: "name" },
      value: { codec: "string" },
      required: true,
      conflict: "explicit"
    },
    "columns.*.brief": {
      path: "columns.*.brief",
      storage: { kind: "text", containerTemplate: "board.column.{column_id}.brief" },
      value: { codec: "string" },
      required: true,
      conflict: "merge"
    },
    "columns.*.tasks": {
      path: "columns.*.tasks",
      storage: {
        kind: "keyedSequence",
        identityPath: "id",
        identityVariable: "task_id",
        orderContainer: "board.column.{column_id}.task_order",
        itemContainerTemplate: "board.column.{column_id}.task.{task_id}"
      },
      value: { codec: "keyedSequence" },
      required: true,
      conflict: "merge"
    },
    "columns.*.tasks.*.id": {
      path: "columns.*.tasks.*.id",
      storage: { kind: "derivedIdentity", identityPath: "id", identityVariable: "task_id" },
      value: { codec: "identity" },
      required: true,
      conflict: "immutable"
    },
    "columns.*.tasks.*.title": {
      path: "columns.*.tasks.*.title",
      storage: {
        kind: "scalar",
        containerTemplate: "board.column.{column_id}.task.{task_id}",
        key: "title"
      },
      value: { codec: "string" },
      required: true,
      conflict: "explicit"
    },
    "columns.*.tasks.*.estimate_hours": {
      path: "columns.*.tasks.*.estimate_hours",
      storage: {
        kind: "scalar",
        containerTemplate: "board.column.{column_id}.task.{task_id}",
        key: "estimate_hours"
      },
      value: { codec: "optionalNumber" },
      required: false,
      conflict: "explicit"
    },
    "columns.*.tasks.*.notes": {
      path: "columns.*.tasks.*.notes",
      storage: {
        kind: "text",
        containerTemplate: "board.column.{column_id}.task.{task_id}.notes"
      },
      value: { codec: "string" },
      required: true,
      conflict: "merge"
    },
    "columns.*.tasks.*.labels": {
      path: "columns.*.tasks.*.labels",
      storage: {
        kind: "orderedList",
        containerTemplate: "board.column.{column_id}.task.{task_id}.labels"
      },
      value: { codec: "stringList" },
      required: true,
      conflict: "merge"
    },
    archive: {
      path: "archive",
      storage: {
        kind: "keyedSequence",
        identityPath: "id",
        identityVariable: "archived_id",
        orderContainer: "board.archive_order",
        itemContainerTemplate: "board.archived.{archived_id}"
      },
      value: { codec: "keyedSequence" },
      required: false,
      conflict: "merge"
    },
    "archive.*.id": {
      path: "archive.*.id",
      storage: { kind: "derivedIdentity", identityPath: "id", identityVariable: "archived_id" },
      value: { codec: "identity" },
      required: false,
      conflict: "immutable"
    },
    "archive.*.title": {
      path: "archive.*.title",
      storage: { kind: "scalar", containerTemplate: "board.archived.{archived_id}", key: "title" },
      value: { codec: "string" },
      required: true,
      conflict: "explicit"
    },
    etag: {
      path: "etag",
      storage: { kind: "derivedRevision" },
      value: { codec: "string" },
      required: true,
      conflict: "immutable"
    }
  }
} as const satisfies CollaborationEntityPlan<"boards.authoringState", "board.importUpdate">;

export interface BoardTask {
  id: string;
  title: string;
  estimateHours?: number;
  notes: string;
  labels: string[];
}

export interface BoardColumn {
  columnId: string;
  name: string;
  brief: string;
  tasks: BoardTask[];
}

export interface BoardDocument {
  boardId: string;
  title: string;
  description?: string;
  columns: BoardColumn[];
  archive?: { id: string; title: string }[];
  etag?: string;
}

export function boardDocument(): BoardDocument {
  return {
    boardId: "board-1",
    title: "Launch",
    description: "Everything before the launch.",
    columns: [
      {
        columnId: "todo",
        name: "To do",
        brief: "Not started.",
        tasks: [
          { id: "task-1", title: "Write copy", estimateHours: 3, notes: "Draft first.", labels: ["copy"] },
          { id: "task-2", title: "Book venue", notes: "Near the station.", labels: [] }
        ]
      },
      {
        columnId: "done",
        name: "Done",
        brief: "Shipped.",
        tasks: [{ id: "task-3", title: "Pick a date", notes: "Friday.", labels: ["planning"] }]
      }
    ]
  };
}

/** The client-side spelling of one wire path segment: snake_case becomes camelCase. */
export function clientSegment(segment: string): string {
  return segment.replace(/_([a-z0-9])/g, (_match, character: string) => character.toUpperCase());
}

/** The value a client document holds at a dotted wire path without `*`. */
export function clientValueAt(document: unknown, wirePath: string): unknown {
  let current: unknown = document;
  for (const segment of wirePath.split(".")) {
    if (current === null || typeof current !== "object") {
      return undefined;
    }
    current = (current as Record<string, unknown>)[clientSegment(segment)];
  }
  return current;
}
