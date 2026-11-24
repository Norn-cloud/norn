-- Track unique attempt id, target environment, and lease start time for pending connector
-- binding saves so reconciliation, finish, and abandon writes match only their originating
-- save attempt, fence active creates and edits during their lease, and never overwrite a newer save.
alter table connector_binding add column pending_attempt text;
alter table connector_binding add column pending_environment_id text;
alter table connector_binding add column pending_started_at integer;
