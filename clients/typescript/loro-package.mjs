/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * Builds the loro-crdt package loro-release.json pins: from the source revision
 * it records, with the tools recorded under npm.build. Writes the tarball and
 * its provenance.json, which carries the tarball's integrity. Checks a built
 * tarball holds the package the record pins.
 */
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

const USAGE = `Usage: node clients/typescript/loro-package.mjs build SOURCE_CHECKOUT OUTPUT_DIRECTORY
       node clients/typescript/loro-package.mjs check TARBALL

build  Builds the loro-crdt package loro-release.json pins. SOURCE_CHECKOUT is a
       clean checkout of the recorded source revision; the first line every
       tool under npm.build.tools prints for --version must be its recorded
       one. Writes the tarball and provenance.json to OUTPUT_DIRECTORY.
check  Exits non-zero unless TARBALL holds the package at the recorded URL,
       whose integrity must be the recorded one.`;

export function readRecord(file = path.join(root, "loro-release.json")) {
  return JSON.parse(readFileSync(file, "utf8"));
}

/** Cargo's home for the build, the same on every machine. */
export const CARGO_HOME = "/tmp/clerkenwell-loro-cargo";

/** The MAJOR.MINOR.PATCH in a tool's --version line. */
export function versionNumber(line) {
  return line.match(/\d+\.\d+\.\d+/)?.[0];
}

/** The environment every build step runs in. */
export function buildEnvironment(record, base = process.env) {
  const env = {
    ...base,
    COREPACK_ENABLE_AUTO_PIN: "0",
    // Loro's checkout names the `stable` toolchain; the record names a version.
    RUSTUP_TOOLCHAIN: versionNumber(record.npm.build.tools.rustc),
    // Panic locations in the module name each dependency's path in Cargo's home.
    CARGO_HOME,
  };
  // Loro's build script posts a size report to a pull request when it finds these.
  for (const key of ["GITHUB_TOKEN", "GITHUB_EVENT_PATH", "CI"]) delete env[key];
  return env;
}

function capture(command, args, cwd, env) {
  const result = spawnSync(command, args, { cwd, env, encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : undefined;
}

/**
 * Refuses a build whose tools report other versions than the record's, or whose
 * source checkout is at another revision or has changes to tracked files.
 */
export function checkInputs(record, source, env) {
  const problems = [];
  for (const [tool, expected] of Object.entries(record.npm.build.tools)) {
    const line = capture(tool, ["--version"], source, env)?.split("\n")[0];
    if (line !== expected) {
      problems.push(`${tool}: the record names ${expected}; found ${line ?? "no working tool"}`);
    }
  }
  const head = capture("git", ["rev-parse", "HEAD"], source, env);
  if (head !== record.source.revision) {
    problems.push(`${source} is at ${head ?? "no Git revision"}; the record names ${record.source.revision}`);
  }
  const status = capture("git", ["status", "--porcelain", "--untracked-files=no"], source, env);
  if (status) problems.push(`${source} has changes to tracked files:\n${status}`);
  if (problems.length > 0) throw new Error(problems.join("\n"));
}

function run(command, args, cwd, env) {
  const result = spawnSync(command, args, { cwd, env, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} exited with ${result.status}`);
}

export function build(record, source, output) {
  const env = buildEnvironment(record);
  checkInputs(record, source, env);
  const wasm = path.join(source, "crates/loro-wasm");
  mkdirSync(output, { recursive: true });

  run("pnpm", ["install", "--frozen-lockfile"], source, env);
  run(
    "deno",
    [
      "run",
      "--allow-read",
      `--allow-write=${source}`,
      "--allow-env",
      "--allow-run=cargo,wasm-bindgen,node",
      "--allow-net=deno.land,registry.npmjs.org",
      "./scripts/build.ts",
      "release",
    ],
    wasm,
    env
  );
  run("pnpm", ["exec", "rollup", "-c"], wasm, env);
  const local = { ...env, PATH: path.join(wasm, "node_modules/.bin") + path.delimiter + env.PATH };
  run(
    "deno",
    ["run", "--allow-read", `--allow-write=${source}`, "--allow-env", "--allow-run=rollup", "./scripts/post-rollup.ts"],
    wasm,
    local
  );
  run("node", ["--no-experimental-require-module", "./scripts/test-commonjs.cjs"], wasm, local);
  run("node", ["--expose-gc", "./node_modules/vitest/vitest.mjs", "run"], wasm, local);
  run("pnpm", ["exec", "tsc", "--noEmit"], wasm, local);
  run("deno", ["test", "-A"], path.join(wasm, "deno_tests"), local);
  run("bun", ["test"], path.join(wasm, "bun_tests"), local);

  const packed = spawnSync("npm", ["pack", "--ignore-scripts", "--json", "--pack-destination", output], {
    cwd: wasm,
    env,
    encoding: "utf8",
  });
  if (packed.status !== 0) throw new Error(`npm pack exited with ${packed.status}: ${packed.stderr}`);
  const artifact = JSON.parse(packed.stdout)[0].filename;
  const integrity = integrityOf(readFileSync(path.join(output, artifact)));
  const provenance = { source: record.source, build: record.npm.build, artifact, integrity };
  writeFileSync(path.join(output, "provenance.json"), JSON.stringify(provenance, null, 2) + "\n");
  return provenance;
}

export function integrityOf(bytes) {
  return "sha512-" + createHash("sha512").update(bytes).digest("base64");
}

/**
 * Whether the built tarball holds the same package as the pinned one, which must
 * carry the recorded integrity. gzip's output depends on the machine that
 * compressed it, so the uncompressed archives are compared.
 */
export function samePackage(record, pinned, built) {
  if (integrityOf(pinned) !== record.npm.integrity) {
    throw new Error(`the pinned tarball's integrity is not the recorded ${record.npm.integrity}`);
  }
  return gunzipSync(pinned).equals(gunzipSync(built));
}

export async function check(record, tarball) {
  const response = await fetch(record.npm.url);
  if (!response.ok) throw new Error(`${record.npm.url} answered ${response.status}`);
  return samePackage(record, Buffer.from(await response.arrayBuffer()), readFileSync(tarball));
}

async function main([command, ...args]) {
  if (command === "build" && args.length === 2) {
    const [source, output] = args.map((arg) => path.resolve(arg));
    console.log(JSON.stringify(build(readRecord(), source, output), null, 2));
  } else if (command === "check" && args.length === 1) {
    const record = readRecord();
    if (!(await check(record, path.resolve(args[0])))) {
      throw new Error(`${args[0]} does not hold the package at ${record.npm.url}`);
    }
    console.log(`${args[0]} holds the package at ${record.npm.url}`);
  } else {
    console.error(USAGE);
    process.exitCode = 2;
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  if (process.argv.includes("--help")) {
    console.log(USAGE);
  } else {
    main(process.argv.slice(2)).catch((error) => {
      console.error(error.message);
      process.exitCode = 1;
    });
  }
}
