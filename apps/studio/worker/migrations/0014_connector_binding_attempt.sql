-- Track unique attempt id, target environment, and lease start time for pending connector
-- binding saves so reconciliation, finish, and abandon writes match only their originating
-- save attempt, fence active creates and edits during their lease, and never overwrite a newer save.
alter table connector_binding add column pending_attempt text;
alter table connector_binding add column pending_environment_id text;
alter table connector_binding add column pending_started_at integer;

-- Durable tombstones for abandoned create attempts whose cell install outcome was unknown (e.g.
-- lost response or timeout): reconciliation continues watching them and retires any late cell
-- commit so an abandoned create can never leave an orphaned definition in the cell.
create table connector_binding_orphan (
  org_id text not null,
  binding_id text not null,
  definition_id text not null,
  environment_id text,
  abandoned_at integer not null,
  primary key (org_id, binding_id)
);
