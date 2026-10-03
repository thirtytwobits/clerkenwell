/**
 * Copyright (c) 2026 Scott A Dixon
 */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { gzipSync } from "node:zlib";
import path from "node:path";
import { afterEach, beforeEach, test } from "node:test";

import {
  CARGO_HOME,
  buildEnvironment,
  checkInputs,
  integrityOf,
  readRecord,
  samePackage,
  versionNumber,
} from "./loro-package.mjs";

let work;
let source;
let tools;

function git(...args) {
  return execFileSync("git", args, { cwd: source, encoding: "utf8" }).trim();
}

/** Puts a tool on the stub path that reports `line` as its version. */
function stub(tool, line) {
  const file = path.join(tools, tool);
  writeFileSync(file, `#!/bin/sh\necho '${line}'\n`);
  chmodSync(file, 0o755);
}

/** The record with its source at the stub checkout's revision, every recorded tool stubbed to report its line. */
function recordAtSource() {
  const record = readRecord();
  record.source = { ...record.source, revision: git("rev-parse", "HEAD") };
  for (const [tool, line] of Object.entries(record.npm.build.tools)) stub(tool, line);
  return record;
}

function environment(record) {
  return buildEnvironment(record, { ...process.env, PATH: tools + path.delimiter + process.env.PATH });
}

beforeEach(() => {
  work = mkdtempSync(path.join(tmpdir(), "clerkenwell-loro-package-"));
  source = path.join(work, "source");
  tools = path.join(work, "tools");
  mkdirSync(source);
  mkdirSync(tools);
  git("init", "--quiet");
  writeFileSync(path.join(source, "tracked"), "one\n");
  git("add", "tracked");
  git("-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "--quiet", "-m", "one");
});

afterEach(() => rmSync(work, { recursive: true, force: true }));

test("a clean checkout at the recorded revision with the recorded tools is accepted", () => {
  const record = recordAtSource();
  writeFileSync(path.join(source, "untracked"), "build output\n");
  checkInputs(record, source, environment(record));
});

test("a tool reporting another version is refused", () => {
  const record = recordAtSource();
  stub("bun", "0.0.1");
  assert.throws(() => checkInputs(record, source, environment(record)), /bun/);
});

test("a tool reporting the recorded version from another build is refused", () => {
  const record = recordAtSource();
  stub("wasm-bindgen", `${record.npm.build.tools["wasm-bindgen"]} (0123abcde)`);
  assert.throws(() => checkInputs(record, source, environment(record)), /wasm-bindgen/);
});

test("a checkout at another revision is refused", () => {
  const record = recordAtSource();
  record.source.revision = "0".repeat(40);
  assert.throws(() => checkInputs(record, source, environment(record)), new RegExp(record.source.revision));
});

test("a checkout with changes to tracked files is refused", () => {
  const record = recordAtSource();
  writeFileSync(path.join(source, "tracked"), "two\n");
  assert.throws(() => checkInputs(record, source, environment(record)), /tracked/);
});

test("a checkout whose status Git cannot report is refused", () => {
  const record = recordAtSource();
  const real = execFileSync("sh", ["-c", "command -v git"], { encoding: "utf8" }).trim();
  const file = path.join(tools, "git");
  writeFileSync(file, `#!/bin/sh\n[ "$1" = status ] && exit 128\nexec '${real}' "$@"\n`);
  chmodSync(file, 0o755);
  assert.throws(() => checkInputs(record, source, environment(record)), /git status failed/);
});

test("the build uses the recorded Rust toolchain and none of the variables Loro reports pull requests with", () => {
  const record = readRecord();
  const env = buildEnvironment(record, { CI: "true", GITHUB_TOKEN: "token", GITHUB_EVENT_PATH: "event", PATH: "" });
  assert.equal(env.RUSTUP_TOOLCHAIN, versionNumber(record.npm.build.tools.rustc));
  for (const key of ["CI", "GITHUB_TOKEN", "GITHUB_EVENT_PATH"]) assert.equal(env[key], undefined);
});

test("the build's Cargo home is the same whatever the builder's home is", () => {
  const record = readRecord();
  const homes = [
    { HOME: "/Users/one", PATH: "" },
    { HOME: "/home/two", CARGO_HOME: "/opt/cargo", PATH: "" },
  ].map((base) => buildEnvironment(record, base).CARGO_HOME);
  assert.deepEqual(homes, [CARGO_HOME, CARGO_HOME]);
  assert.ok(path.isAbsolute(CARGO_HOME));
});

test("a tarball holding the pinned package is the same package however it was compressed", () => {
  const archive = Buffer.from("package/package.json\n{}\n".repeat(64));
  const pinned = gzipSync(archive, { level: 9 });
  const record = { npm: { integrity: integrityOf(pinned) } };
  assert.equal(samePackage(record, pinned, gzipSync(archive, { level: 1 })), true);
  assert.equal(samePackage(record, pinned, gzipSync(Buffer.concat([archive, Buffer.from("x")]))), false);
});

test("a pinned tarball without the recorded integrity is refused", () => {
  const pinned = gzipSync(Buffer.from("package"));
  const record = { npm: { integrity: integrityOf(Buffer.from("another")) } };
  assert.throws(() => samePackage(record, pinned, pinned), /integrity/);
});
