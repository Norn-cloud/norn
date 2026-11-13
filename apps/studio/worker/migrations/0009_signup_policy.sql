-- Who may create an account on this deployment. With no row (every existing stage), sign-up is
-- open, as before. A self-hosted install writes 'first-owner' once at setup: the first account
-- becomes its workspace's owner, and after that an account is created only for an address with a
-- pending, unexpired invitation. The trigger decides inside the insert, so two concurrent sign-ups
-- on a fresh install cannot both pass a check made before either commits.
create table "studio_setting" (
  "key" text not null primary key,
  "value" text not null,
  check ("key" <> 'signup' or "value" in ('open', 'first-owner'))
);
create trigger "studio_signup_policy" before insert on "user"
when (select "value" from "studio_setting" where "key" = 'signup') = 'first-owner'
  and exists (select 1 from "user")
  and not exists (
    select 1 from "invitation"
    where lower("email") = lower(new."email") and "status" = 'pending'
      -- Better Auth writes dates as ISO-8601 text; epoch milliseconds are honoured too.
      and case typeof("expiresAt")
        when 'text' then "expiresAt" > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        when 'integer' then "expiresAt" > unixepoch('now') * 1000
        else 0 end
  )
begin
  select raise(abort, 'signup.invite_only');
end;
