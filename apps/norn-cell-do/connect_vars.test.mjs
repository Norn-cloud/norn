// OLAP-01: the analytics signing key and the other deployment-wide bindings
// never reach the cell's connections. Run: bun test ./connect_vars.test.mjs
// (from apps/norn-cell-do).
import { describe, expect, test } from "bun:test";
import { connectVars, OLAP_SIGNING_KEY } from "./connect_vars.mjs";

const SIGNING = "k1:c2lnbmluZy1zZWVkLW5ldmVyLWZvci1jb25uZWN0b3Jz";
const LINEAGE = "lineage-key-of-at-least-thirty-two-bytes";

const env = {
  [OLAP_SIGNING_KEY]: SIGNING,
  NORN_LINEAGE_KEY: LINEAGE,
  NORN_OLAP_URL: "http://olapd.internal:8790",
  NORN_SOURCES: "[]",
  NORN_S3_ENDPOINT: "https://storage.example.test",
  NORN_S3_SECRET_ACCESS_KEY: "storage-secret",
  NORN_SECRET_ERPNEXT_REST_TOKEN: "connection-token",
  NORN_SOURCE_ALLOWED_HOSTS: "erp.example.test",
  CELLS: { idFromName() {} },
};

describe("connectVars", () => {
  test("the signing key is in no variable the connections receive", () => {
    const vars = JSON.parse(connectVars(env));
    expect(Object.values(vars)).not.toContain(SIGNING);
    expect(Object.values(vars)).not.toContain(LINEAGE);
    expect(JSON.stringify(vars)).not.toContain(SIGNING.split(":")[1]);
    expect(Object.keys(vars).some((name) => name.includes("OLAP"))).toBe(false);
  });

  test("the signing key's binding is outside every namespace connections read", () => {
    expect(OLAP_SIGNING_KEY.startsWith("NORN_SECRET_")).toBe(false);
    expect(OLAP_SIGNING_KEY.startsWith("NORN_S3_")).toBe(false);
    expect(JSON.parse(connectVars({ [OLAP_SIGNING_KEY]: SIGNING }))).toEqual({});
  });

  test("connections get their secrets, storage settings and the host allowlist only", () => {
    expect(Object.keys(JSON.parse(connectVars(env))).sort()).toEqual([
      "NORN_S3_ENDPOINT",
      "NORN_S3_SECRET_ACCESS_KEY",
      "NORN_SECRET_ERPNEXT_REST_TOKEN",
      "NORN_SOURCE_ALLOWED_HOSTS",
    ]);
  });
});
