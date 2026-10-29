CREATE TABLE IF NOT EXISTS "o:commerce.PurchaseOrder" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "po_number" TEXT NOT NULL,
  "status" TEXT NOT NULL,
  "payment_status" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:commerce.PurchaseOrder/key:primary" ON "o:commerce.PurchaseOrder" ("_scope", "_world", "po_number");
CREATE TABLE IF NOT EXISTS "o:commerce.SalesOrder" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "order_number" TEXT NOT NULL,
  "total" TEXT NOT NULL,
  "currency" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:commerce.SalesOrder/key:primary" ON "o:commerce.SalesOrder" ("_scope", "_world", "order_number");
CREATE TABLE IF NOT EXISTS "o:commerce.SalesOrderLine" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "order_number" TEXT NOT NULL,
  "line_number" INTEGER NOT NULL,
  "quantity" TEXT NOT NULL,
  "unit_price" TEXT NOT NULL,
  "currency" TEXT NOT NULL,
  "status" TEXT NOT NULL,
  "requested_on" TEXT,
  "promised_at" TEXT,
  "cutoff_at" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:commerce.SalesOrderLine/key:primary" ON "o:commerce.SalesOrderLine" ("_scope", "_world", "order_number", "line_number");
CREATE TABLE IF NOT EXISTS "o:crm.BillingProfile" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "invoice_email" TEXT NOT NULL,
  "payment_terms_days" INTEGER NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE TABLE IF NOT EXISTS "o:crm.Customer" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "customer_number" TEXT NOT NULL,
  "display_name" TEXT NOT NULL,
  "email" TEXT,
  "tier" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:crm.Customer/key:primary" ON "o:crm.Customer" ("_scope", "_world", "customer_number");
CREATE TABLE IF NOT EXISTS "o:crm.Party" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "party_number" TEXT NOT NULL,
  "email" TEXT NOT NULL,
  "display_name" TEXT NOT NULL,
  "segment" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:crm.Party/key:primary" ON "o:crm.Party" ("_scope", "_world", "party_number");
CREATE UNIQUE INDEX IF NOT EXISTS "o:crm.Party/key:by_email" ON "o:crm.Party" ("_scope", "_world", "email");
CREATE TABLE IF NOT EXISTS "o:hr.Employee" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "employee_number" TEXT NOT NULL,
  "display_name" TEXT NOT NULL,
  "email" TEXT NOT NULL,
  "hired_on" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:hr.Employee/key:primary" ON "o:hr.Employee" ("_scope", "_world", "employee_number");
CREATE TABLE IF NOT EXISTS "o:meta.Table" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "qualified_name" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:meta.Table/key:primary" ON "o:meta.Table" ("_scope", "_world", "qualified_name");
CREATE TABLE IF NOT EXISTS "o:projects.Project" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "code" TEXT NOT NULL,
  "title" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:projects.Project/key:primary" ON "o:projects.Project" ("_scope", "_world", "code");
CREATE TABLE IF NOT EXISTS "o:retail.Sku" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "sku_code" TEXT NOT NULL,
  "title" TEXT NOT NULL,
  "lifecycle" TEXT NOT NULL,
  "is_discontinued" INTEGER NOT NULL,
  "net_weight" TEXT NOT NULL,
  "reorder_quantity" INTEGER NOT NULL,
  "channel_metadata" TEXT,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:retail.Sku/key:primary" ON "o:retail.Sku" ("_scope", "_world", "sku_code");
CREATE TABLE IF NOT EXISTS "o:acme.WholesaleCustomer" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "acme__credit_limit" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id"),
  FOREIGN KEY ("_world", "_id") REFERENCES "o:crm.Customer" ("_world", "_id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "o:commerce.RushPurchaseOrder" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id"),
  FOREIGN KEY ("_world", "_id") REFERENCES "o:commerce.PurchaseOrder" ("_world", "_id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "o:crm.KeyAccount" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "account_manager" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id"),
  FOREIGN KEY ("_world", "_id") REFERENCES "o:crm.Customer" ("_world", "_id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "l:commerce.SalesOrder.backed_by" (
  "_world" TEXT NOT NULL,
  "_source" TEXT NOT NULL,
  "_target" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_source", "_target"),
  FOREIGN KEY ("_world", "_source") REFERENCES "o:commerce.SalesOrder" ("_world", "_id") ON DELETE CASCADE,
  FOREIGN KEY ("_world", "_target") REFERENCES "o:meta.Table" ("_world", "_id") ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS "l:commerce.SalesOrder.backed_by/target" ON "l:commerce.SalesOrder.backed_by" ("_world", "_target");
CREATE TABLE IF NOT EXISTS "l:commerce.SalesOrder.customer" (
  "_world" TEXT NOT NULL,
  "_source" TEXT NOT NULL,
  "_target" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_source", "_target"),
  FOREIGN KEY ("_world", "_source") REFERENCES "o:commerce.SalesOrder" ("_world", "_id") ON DELETE CASCADE,
  FOREIGN KEY ("_world", "_target") REFERENCES "o:crm.Customer" ("_world", "_id") ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS "l:commerce.SalesOrder.customer/target" ON "l:commerce.SalesOrder.customer" ("_world", "_target");
CREATE UNIQUE INDEX IF NOT EXISTS "l:commerce.SalesOrder.customer/source" ON "l:commerce.SalesOrder.customer" ("_world", "_source");
CREATE TABLE IF NOT EXISTS "l:crm.BillingProfile.account" (
  "_world" TEXT NOT NULL,
  "_source" TEXT NOT NULL,
  "_target" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_source", "_target"),
  FOREIGN KEY ("_world", "_source") REFERENCES "o:crm.BillingProfile" ("_world", "_id") ON DELETE CASCADE,
  FOREIGN KEY ("_world", "_target") REFERENCES "o:crm.Customer" ("_world", "_id") ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS "l:crm.BillingProfile.account/target" ON "l:crm.BillingProfile.account" ("_world", "_target");
CREATE UNIQUE INDEX IF NOT EXISTS "l:crm.BillingProfile.account/source" ON "l:crm.BillingProfile.account" ("_world", "_source");
CREATE TABLE IF NOT EXISTS "l:meta.Table.derived_from" (
  "_world" TEXT NOT NULL,
  "_source" TEXT NOT NULL,
  "_target" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_source", "_target"),
  FOREIGN KEY ("_world", "_source") REFERENCES "o:meta.Table" ("_world", "_id") ON DELETE CASCADE,
  FOREIGN KEY ("_world", "_target") REFERENCES "o:meta.Table" ("_world", "_id") ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS "l:meta.Table.derived_from/target" ON "l:meta.Table.derived_from" ("_world", "_target");
CREATE TABLE IF NOT EXISTS "l:projects.Project.members" (
  "_world" TEXT NOT NULL,
  "_source" TEXT NOT NULL,
  "_target" TEXT NOT NULL,
  "role" TEXT NOT NULL,
  "allocation" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_source", "_target"),
  FOREIGN KEY ("_world", "_source") REFERENCES "o:projects.Project" ("_world", "_id") ON DELETE CASCADE,
  FOREIGN KEY ("_world", "_target") REFERENCES "o:hr.Employee" ("_world", "_id") ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS "l:projects.Project.members/target" ON "l:projects.Project.members" ("_world", "_target");
