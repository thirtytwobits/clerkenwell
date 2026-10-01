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

interface EntryPoints {
  checker: ts.TypeChecker;
  program: ts.Program;
  /** Each entry point's specifier and module symbol. */
  modules: Array<readonly [string, ts.Symbol]>;
}

function entryPoints(packageDirectory: string): EntryPoints {
  const manifest = JSON.parse(readFileSync(path.join(packageDirectory, "package.json"), "utf8")) as {
    exports: Record<string, { "@clerkenwell/source": string }>;
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
  // Each entry point's source, which its published declarations are built from.
  const entries = Object.entries(manifest.exports).map(
    ([specifier, conditions]) => [specifier, path.resolve(packageDirectory, conditions["@clerkenwell/source"])] as const
  );
  const program = ts.createProgram(entries.map(([, file]) => file), config.options);
  const checker = program.getTypeChecker();
  const modules = entries.map(([specifier, file]) => {
    const source = program.getSourceFile(file);
    const module = source === undefined ? undefined : checker.getSymbolAtLocation(source);
    if (module === undefined) {
      throw new Error(`${file} is not a module.`);
    }
    return [specifier, module] as const;
  });
  return { checker, program, modules };
}

/** Every name each of the package's entry points exports, sorted. */
export function exportedNames(packageDirectory: string): Record<string, string[]> {
  const { checker, modules } = entryPoints(packageDirectory);
  return Object.fromEntries(
    modules.map(([specifier, module]) => [
      specifier,
      checker.getExportsOfModule(module).map((symbol) => symbol.name).sort()
    ])
  );
}

/** Every entry point, and every name one exports, that mentions Loro. */
export function namesAfterLoro(packageDirectory: string): string[] {
  return Object.entries(exportedNames(packageDirectory)).flatMap(([entry, names]) =>
    [entry, ...names].filter((name) => /loro/i.test(name)).map((name) => `${entry}: ${name}`)
  );
}

/** The installed package a declaration file belongs to, if any. */
function installedPackage(fileName: string): string | undefined {
  const installed = fileName.split("/node_modules/").slice(1).at(-1);
  if (installed === undefined) {
    return undefined;
  }
  const [first = "", second = ""] = installed.split("/");
  return first.startsWith("@") ? `${first}/${second}` : first;
}

/** Whether a consumer's declarations leave `node`'s type out: a private or protected member. */
function isHidden(node: ts.Node): boolean {
  const modifiers = ts.canHaveModifiers(node) ? ts.getModifiers(node) ?? [] : [];
  const name = (node as { name?: ts.Node }).name;
  return (
    modifiers.some(
      (modifier) => modifier.kind === ts.SyntaxKind.PrivateKeyword || modifier.kind === ts.SyntaxKind.ProtectedKeyword
    ) ||
    (name !== undefined && ts.isPrivateIdentifier(name))
  );
}

/**
 * The installed packages whose types the entry points' exports name, as a
 * consumer's declarations would show them: every type position of a public
 * declaration, and the inferred type where it has none.
 */
export function packagesNamedByExports(packageDirectory: string): Set<string> {
  const { checker, program, modules } = entryPoints(packageDirectory);
  const packages = new Set<string>();
  const visited = new Set<ts.Symbol>();

  const visitSymbol = (symbol: ts.Symbol | undefined): void => {
    if (symbol === undefined) {
      return;
    }
    const target = (symbol.flags & ts.SymbolFlags.Alias) !== 0 ? checker.getAliasedSymbol(symbol) : symbol;
    if (visited.has(target)) {
      return;
    }
    visited.add(target);
    for (const declaration of target.declarations ?? []) {
      const source = declaration.getSourceFile();
      if (program.isSourceFileDefaultLibrary(source)) {
        continue;
      }
      const installed = installedPackage(source.fileName);
      if (installed === undefined) {
        visitDeclaration(declaration);
      } else {
        packages.add(installed);
      }
    }
  };

  const visitType = (type: ts.Type, depth = 0): void => {
    if (depth > 4) {
      return;
    }
    visitSymbol(type.aliasSymbol);
    visitSymbol(type.getSymbol());
    for (const argument of type.aliasTypeArguments ?? []) {
      visitType(argument, depth + 1);
    }
    if (type.isUnionOrIntersection()) {
      type.types.forEach((member) => visitType(member, depth + 1));
    } else if (
      (type.flags & ts.TypeFlags.Object) !== 0 &&
      ((type as ts.ObjectType).objectFlags & ts.ObjectFlags.Reference) !== 0
    ) {
      checker.getTypeArguments(type as ts.TypeReference).forEach((argument) => visitType(argument, depth + 1));
    }
  };

  const visitDeclaration = (node: ts.Node): void => {
    if (isHidden(node)) {
      return;
    }
    if (ts.isTypeNode(node)) {
      visitType(checker.getTypeFromTypeNode(node));
    }
    if (ts.isFunctionLike(node) && !ts.isConstructorDeclaration(node) && node.type === undefined) {
      const signature = checker.getSignatureFromDeclaration(node);
      if (signature !== undefined) {
        visitType(signature.getReturnType());
      }
    }
    if ((ts.isVariableDeclaration(node) || ts.isPropertyDeclaration(node)) && node.type === undefined) {
      visitType(checker.getTypeAtLocation(node));
    }
    ts.forEachChild(node, (child) => {
      const initializer = (node as { initializer?: ts.Node }).initializer;
      if (ts.isBlock(child) || child === initializer) {
        return;
      }
      visitDeclaration(child);
    });
  };

  for (const [, module] of modules) {
    for (const exported of checker.getExportsOfModule(module)) {
      visitSymbol(exported);
    }
  }
  return packages;
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
