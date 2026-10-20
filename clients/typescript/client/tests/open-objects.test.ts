/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A generated open object holds its declared members, each of its own type
 * and optional where the schema leaves it so, beside other members of the
 * other members' type.
 */
import assert from "node:assert/strict";
import test from "node:test";

import type * as Notebook from "../../../../crates/clerkenwell-notebook/generated/typescript/index.js";

test("an open object holds its declared members and other members of their type", () => {
  const appearances: Notebook.NoteAppearance[] = [
    { theme: "light" },
    { theme: "dark", accent: "teal", density: "compact" },
    { theme: "light", accent: undefined },
  ];
  // @ts-expect-error an other member is a value of the other members' type
  const numbered: Notebook.NoteAppearance = { theme: "light", density: 1 };
  // @ts-expect-error a declared member keeps its own type
  const unthemed: Notebook.NoteAppearance = { theme: "blue" };
  assert.equal(appearances.length + [numbered, unthemed].length, 5);
});
