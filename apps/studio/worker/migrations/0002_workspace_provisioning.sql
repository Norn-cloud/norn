-- A completed bootstrap remains complete even if the user is later removed.
create table "workspace_provisioning" (
  "userId" text not null primary key references "user" ("id") on delete cascade,
  "status" text not null check ("status" in ('pending', 'complete'))
);
create unique index "member_organization_user_unique" on "member" ("organizationId", "userId");
