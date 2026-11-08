-- A tool binding's cell definition (#772): DEFS installs each binding as `kind: "binding"`, and
-- agents reference it by this id. Both stay null while a stage keeps bindings locally only.
ALTER TABLE "connector_binding" ADD COLUMN "definition_id" text;
ALTER TABLE "connector_binding" ADD COLUMN "definition_version" integer;
