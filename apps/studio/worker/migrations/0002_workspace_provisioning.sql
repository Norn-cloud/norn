-- A completed bootstrap remains complete even if the user is later removed.
create table "workspace_provisioning" (
  "userId" text not null primary key references "user" ("id") on delete cascade,
  "status" text not null check ("status" in ('pending', 'complete'))
);
-- The marker and new user commit together. Existing users are not backfilled:
-- a missing marker for them must never recreate a deliberately removed owner.
create trigger "workspace_provisioning_on_user_insert" after insert on "user"
begin
  insert into "workspace_provisioning" ("userId", "status") values (new."id", 'pending');
end;
-- Keep the highest role, then the oldest membership, then the lowest id.
delete from "member" where "id" in (
  select "id" from (
    select "id", row_number() over (
      partition by "organizationId", "userId"
      order by case "role" when 'owner' then 4 when 'admin' then 3 when 'member' then 2 when 'viewer' then 1 else 0 end desc,
               "createdAt" asc, "id" asc
    ) as position from "member"
  ) where position > 1
);
create unique index "member_organization_user_unique" on "member" ("organizationId", "userId");
