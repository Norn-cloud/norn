-- Pending save attempt isolation for connector bindings (S2):
-- Persist the unique save attempt id and target cell environment on pending rows so reconciliation,
-- finish, and abandon writes match only the exact pending attempt and can never overwrite a newer save.
ALTER TABLE "connector_binding" ADD COLUMN "pending_attempt" text;
ALTER TABLE "connector_binding" ADD COLUMN "pending_environment_id" text;
