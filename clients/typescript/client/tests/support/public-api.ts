/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * What a package's entry points export, as the TypeScript compiler resolves
 * them, against the names its `public-api.json` records.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import ts from "typescript";

/** Every name each of the package's entry points exports, sorted. */
export function exportedNames(packageDirectory: string): Record<string, string[]> {
  const manifest = JSON.parse(readFileSync(path.join(packageDirectory, "package.json"), "utf8")) as {
    exports: Record<string, string>;
  };
  const configPath = path.join(packageDirectory, "tsconfig.json");
  const config = ts.getParsedCommandLineOfConfigFile(configPath, {}, {
    ...ts.sys,
    onUnRecoverableConfigFileDiagnostic: (diagnostic) => {
      throw new Error(ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"));
    }
  });
  if (config === undefined) {
    throw new Error(`${configPath} could not be read.`);
  }
  const entries = Object.entries(manifest.exports).map(
    ([specifier, file]) => [specifier, path.resolve(packageDirectory, file)] as const
  );
  const program = ts.createProgram(entries.map(([, file]) => file), config.options);
  const checker = program.getTypeChecker();
  return Object.fromEntries(
    entries.map(([specifier, file]) => {
      const source = program.getSourceFile(file);
      const module = source === undefined ? undefined : checker.getSymbolAtLocation(source);
      if (module === undefined) {
        throw new Error(`${file} is not a module.`);
      }
      return [specifier, checker.getExportsOfModule(module).map((symbol) => symbol.name).sort()];
    })
  );
}

/** Refuses any difference between what the package exports and what it records. */
export function assertRecordedPublicApi(packageDirectory: string): void {
  const recorded = JSON.parse(
    readFileSync(path.join(packageDirectory, "public-api.json"), "utf8")
  ) as Record<string, string[]>;
  const exported = exportedNames(packageDirectory);
  const differences = Object.keys({ ...recorded, ...exported }).flatMap((entry) => {
    const now = new Set(exported[entry] ?? []);
    const before = new Set(recorded[entry] ?? []);
    const added = [...now].filter((name) => !before.has(name));
    const removed = [...before].filter((name) => !now.has(name));
    return [
      ...added.map((name) => `${entry}: exports ${name}, which public-api.json does not record`),
      ...removed.map((name) => `${entry}: public-api.json records ${name}, which is not exported`)
    ];
  });
  assert.deepEqual(
    differences,
    [],
    "The public surface differs from public-api.json; change the record only when the surface is meant to change."
  );
}
