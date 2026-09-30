/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * The client's entry points export exactly the names its record declares.
 */
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { assertRecordedPublicApi } from "./support/public-api";

test("the client's entry points export exactly what public-api.json records", () => {
  assertRecordedPublicApi(path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."));
});
