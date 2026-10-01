/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Checks the TypeScript packages as a consumer receives them. Builds and packs
 * each, lints the tarballs with publint and arethetypeswrong, installs them
 * into an empty project, imports every entry point under plain Node, and
 * typechecks two consumers under NodeNext resolution: a Node command-line tool
 * using the client, and a web application using all three entry points.
 */
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const bin = (name) => path.join(root, "node_modules", ".bin", name);
const manifest = (directory) => JSON.parse(readFileSync(path.join(root, directory, "package.json"), "utf8"));

const PACKAGES = ["clients/typescript/client", "clients/typescript/react"];
const ENTRY_POINTS = ["@clerkenwell/client", "@clerkenwell/client/replica", "@clerkenwell/react"];

function run(command, args, cwd) {
  execFileSync(command, args, { cwd, stdio: "inherit" });
}

const work = mkdtempSync(path.join(tmpdir(), "clerkenwell-package-check-"));
try {
  const tarballs = [];
  for (const directory of PACKAGES) {
    run("npm", ["run", "build"], path.join(root, directory));
    const packed = execFileSync(
      "npm",
      ["pack", "--ignore-scripts", "--json", "--pack-destination", work],
      { cwd: path.join(root, directory), encoding: "utf8" }
    );
    tarballs.push(path.join(work, JSON.parse(packed)[0].filename));
  }

  for (const tarball of tarballs) {
    run(bin("publint"), ["run", tarball, "--strict"], root);
    run(bin("attw"), [tarball, "--profile", "esm-only"], root);
  }

  // The consumer installs what the packages declare, plus the versions this
  // workspace develops against for React and the Node and React types.
  const workspace = manifest(".").devDependencies;
  const react = manifest("clients/typescript/react").devDependencies;
  const consumer = path.join(work, "consumer");
  mkdirSync(consumer);
  writeFileSync(
    path.join(consumer, "package.json"),
    JSON.stringify({ name: "consumer", private: true, type: "module" }, null, 2)
  );
  run(
    "npm",
    [
      "install",
      "--no-audit",
      "--no-fund",
      "--prefer-offline",
      ...tarballs,
      `react@${react.react}`,
      `react-dom@${react["react-dom"]}`,
      `@types/react@${react["@types/react"]}`,
      `@types/node@${workspace["@types/node"]}`
    ],
    consumer
  );

  for (const entry of ENTRY_POINTS) {
    const names = execFileSync(
      "node",
      ["--input-type=module", "-e", `console.log(Object.keys(await import(${JSON.stringify(entry)})).length)`],
      { cwd: consumer, encoding: "utf8" }
    );
    if (Number(names) === 0) {
      throw new Error(`${entry} imports under Node but exports nothing.`);
    }
  }

  const consumers = {
    cli: { entries: ["@clerkenwell/client", "@clerkenwell/client/replica"], lib: ["ES2023"], types: ["node"] },
    web: { entries: ENTRY_POINTS, lib: ["ES2023", "DOM"], types: [] }
  };
  for (const [name, { entries, lib, types }] of Object.entries(consumers)) {
    const source = entries.map((entry, index) => `import * as entry${index} from ${JSON.stringify(entry)};`);
    source.push(`export const entries = [${entries.map((_, index) => `entry${index}`).join(", ")}];`);
    writeFileSync(path.join(consumer, `${name}.ts`), `${source.join("\n")}\n`);
    writeFileSync(
      path.join(consumer, `tsconfig.${name}.json`),
      JSON.stringify(
        {
          compilerOptions: {
            target: "ES2022",
            module: "NodeNext",
            moduleResolution: "NodeNext",
            lib,
            types,
            strict: true,
            noEmit: true,
            skipLibCheck: true
          },
          files: [`${name}.ts`]
        },
        null,
        2
      )
    );
    run(bin("tsc"), ["-p", `tsconfig.${name}.json`], consumer);
  }
  console.log("The packages install, import and typecheck as a consumer receives them.");
} finally {
  rmSync(work, { recursive: true, force: true });
}
