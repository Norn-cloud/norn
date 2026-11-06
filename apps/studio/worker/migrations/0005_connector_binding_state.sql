-- A binding's save state (#1037): 'creating' or 'editing' while a save is between the cell and this row, or did
-- not finish; such a row neither runs nor changes until reconciled against the cell. 'ready' otherwise.
ALTER TABLE "connector_binding" ADD COLUMN "state" text NOT NULL DEFAULT 'ready' CHECK ("state" IN ('ready', 'creating', 'editing'));
