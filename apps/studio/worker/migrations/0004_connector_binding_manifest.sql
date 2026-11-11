-- Each tool binding records the tool manifest it was saved against (#772): the canvas's agent
-- check and the host's admission identify a bound tool by this digest, never by its name.
-- `connector` is JSON {gateway, origin, app, tool, version}. Both are recomputed on every save.
ALTER TABLE "connector_binding" ADD COLUMN "manifest_digest" text NOT NULL DEFAULT '';
ALTER TABLE "connector_binding" ADD COLUMN "connector" text NOT NULL DEFAULT '{}';
