CREATE TABLE IF NOT EXISTS "o:commerce.PurchaseOrder" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:po_number" TEXT NOT NULL,
  "p:status" TEXT NOT NULL,
  "p:payment_status" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:commerce.PurchaseOrder/key:primary" ON "o:commerce.PurchaseOrder" ("_scope", "_world", "p:po_number");
CREATE TABLE IF NOT EXISTS "o:commerce.SalesOrder" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:order_number" TEXT NOT NULL,
  "p:total" TEXT NOT NULL,
  "p:currency" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:commerce.SalesOrder/key:primary" ON "o:commerce.SalesOrder" ("_scope", "_world", "p:order_number");
CREATE TABLE IF NOT EXISTS "o:commerce.SalesOrderLine" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:order_number" TEXT NOT NULL,
  "p:line_number" INTEGER NOT NULL,
  "p:quantity" TEXT NOT NULL,
  "p:unit_price" TEXT NOT NULL,
  "p:currency" TEXT NOT NULL,
  "p:status" TEXT NOT NULL,
  "p:requested_on" TEXT,
  "p:promised_at" TEXT,
  "p:cutoff_at" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:commerce.SalesOrderLine/key:primary" ON "o:commerce.SalesOrderLine" ("_scope", "_world", "p:order_number", "p:line_number");
CREATE TABLE IF NOT EXISTS "o:crm.BillingProfile" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:invoice_email" TEXT NOT NULL,
  "p:payment_terms_days" INTEGER NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE TABLE IF NOT EXISTS "o:crm.Customer" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:customer_number" TEXT NOT NULL,
  "p:display_name" TEXT NOT NULL,
  "p:email" TEXT,
  "p:tier" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:crm.Customer/key:primary" ON "o:crm.Customer" ("_scope", "_world", "p:customer_number");
CREATE TABLE IF NOT EXISTS "o:crm.Party" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:party_number" TEXT NOT NULL,
  "p:email" TEXT NOT NULL,
  "p:display_name" TEXT NOT NULL,
  "p:segment" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:crm.Party/key:primary" ON "o:crm.Party" ("_scope", "_world", "p:party_number");
CREATE UNIQUE INDEX IF NOT EXISTS "o:crm.Party/key:by_email" ON "o:crm.Party" ("_scope", "_world", "p:email");
CREATE TABLE IF NOT EXISTS "o:hr.Employee" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:employee_number" TEXT NOT NULL,
  "p:display_name" TEXT NOT NULL,
  "p:email" TEXT NOT NULL,
  "p:hired_on" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:hr.Employee/key:primary" ON "o:hr.Employee" ("_scope", "_world", "p:employee_number");
CREATE TABLE IF NOT EXISTS "o:meta.Table" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:qualified_name" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:meta.Table/key:primary" ON "o:meta.Table" ("_scope", "_world", "p:qualified_name");
CREATE TABLE IF NOT EXISTS "o:projects.Project" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:code" TEXT NOT NULL,
  "p:title" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:projects.Project/key:primary" ON "o:projects.Project" ("_scope", "_world", "p:code");
CREATE TABLE IF NOT EXISTS "o:retail.Sku" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_type" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "_version" INTEGER NOT NULL,
  "p:sku_code" TEXT NOT NULL,
  "p:title" TEXT NOT NULL,
  "p:lifecycle" TEXT NOT NULL,
  "p:is_discontinued" INTEGER NOT NULL,
  "p:net_weight" TEXT NOT NULL,
  "p:reorder_quantity" INTEGER NOT NULL,
  "p:channel_metadata" TEXT,
  PRIMARY KEY ("_world", "_id")
);
CREATE UNIQUE INDEX IF NOT EXISTS "o:retail.Sku/key:primary" ON "o:retail.Sku" ("_scope", "_world", "p:sku_code");
CREATE TABLE IF NOT EXISTS "o:acme.WholesaleCustomer" (
  "_world" TEXT NOT NULL,
  "_id" TEXT NOT NULL,
  "_scope" TEXT NOT NULL,
  "p:acme__credit_limit" TEXT NOT NULL,
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
  "p:account_manager" TEXT NOT NULL,
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
  "p:role" TEXT NOT NULL,
  "p:allocation" TEXT NOT NULL,
  PRIMARY KEY ("_world", "_source", "_target"),
  FOREIGN KEY ("_world", "_source") REFERENCES "o:projects.Project" ("_world", "_id") ON DELETE CASCADE,
  FOREIGN KEY ("_world", "_target") REFERENCES "o:hr.Employee" ("_world", "_id") ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS "l:projects.Project.members/target" ON "l:projects.Project.members" ("_world", "_target");
