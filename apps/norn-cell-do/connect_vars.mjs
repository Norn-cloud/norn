// OLAP-01: what the Worker hands the cell's connections and uploads, by name.

/** The binding of the analytics signing key; the Worker reads it for `enableAnalytics` only. */
export const OLAP_SIGNING_KEY = "NORN_OLAP_SIGNING_KEY";

/**
 * The server variables the cell's connections and uploads read, by name, as
 * one JSON object: connection secrets (`NORN_SECRET_*`, which a connector's
 * auth may name and send to its source), storage settings and keys
 * (`NORN_S3_*`, read by the upload path only) and the operator's source host
 * allowlist (SYNC-URL-1). Nothing else: the analytics signing key
 * (`NORN_OLAP_SIGNING_KEY`), the lineage key and every other binding stay
 * out. Never logged.
 */
export function connectVars(env) {
  return JSON.stringify(
    Object.fromEntries(
      Object.entries(env).filter(
        ([name, value]) =>
          typeof value === "string" && (name.startsWith("NORN_SECRET_") || name.startsWith("NORN_S3_") || name === "NORN_SOURCE_ALLOWED_HOSTS"),
      ),
    ),
  );
}
