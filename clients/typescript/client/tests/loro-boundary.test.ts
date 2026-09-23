/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The CRDT runtime stays behind the `/loro` entry point: one module imports
 * `loro-crdt`, and nothing the main entry reaches imports that module.
 */
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import ts from "typescript";

const PACKAGE_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE_DIR = path.join(PACKAGE_DIR, "src");

interface PackageManifest {
  exports: Record<string, string>;
}

async function manifest(): Promise<PackageManifest> {
  return JSON.parse(await readFile(path.join(PACKAGE_DIR, "package.json"), "utf8")) as PackageManifest;
}

async function importsOf(file: string): Promise<string[]> {
  const source = await readFile(file, "utf8");
  return ts.preProcessFile(source, true, true).importedFiles.map(({ fileName }) => fileName);
}

function isLoro(specifier: string): boolean {
  return specifier === "loro-crdt" || specifier.startsWith("loro-crdt/");
}

async function reachable(entry: string): Promise<Map<string, string[]>> {
  const graph = new Map<string, string[]>();
  const pending = [entry];
  while (pending.length > 0) {
    const file = pending.pop()!;
    if (graph.has(file)) {
      continue;
    }
    const specifiers = await importsOf(file);
    graph.set(file, specifiers);
    for (const specifier of specifiers) {
      if (specifier.startsWith(".")) {
        pending.push(`${path.resolve(path.dirname(file), specifier)}.ts`);
      }
    }
  }
  return graph;
}

test("only the module behind the /loro entry imports loro-crdt", async () => {
  const { exports } = await manifest();
  const loroEntry = path.resolve(PACKAGE_DIR, exports["./loro"]!);
  const importers: string[] = [];
  for (const name of await readdir(SOURCE_DIR)) {
    const file = path.join(SOURCE_DIR, name);
    if ((await importsOf(file)).some(isLoro)) {
      importers.push(file);
    }
  }

  assert.deepEqual(importers, [loroEntry]);
});

test("nothing the main entry reaches imports loro-crdt or the /loro module", async () => {
  const { exports } = await manifest();
  const mainEntry = path.resolve(PACKAGE_DIR, exports["."]!);
  const loroEntry = path.resolve(PACKAGE_DIR, exports["./loro"]!);

  const graph = await reachable(mainEntry);

  assert.ok(graph.size > 1, "The main entry's import graph was not followed.");
  assert.equal(graph.has(loroEntry), false);
  for (const [file, specifiers] of graph) {
    assert.deepEqual(specifiers.filter(isLoro), [], path.relative(PACKAGE_DIR, file));
  }
});
