-- Commit-time OAuth membership & session guards (S3):
-- 1. No oauthConsent row can be inserted or updated unless the user holds a live membership
--    in a ready (non-provisioning) organization matching referenceId.
-- 2. No authorization_code verification row can be inserted unless the user holds a live
--    membership in a ready organization matching referenceId AND the authorizing session
--    still exists and has not expired.
-- 3. No oauthRefreshToken or user oauthAccessToken can be persisted after concurrent revocation.
-- 4. Deleting a membership immediately removes matching OAuth consents and revokes matching
--    OAuth refresh and access tokens so a later rejoin can never resurrect a prior grant.

create trigger "oauth_consent_requires_ready_member_insert" before insert on "oauthConsent"
begin
  select raise(abort, 'oauth.membership_invalid')
  where new."userId" is null
     or new."referenceId" is null
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = new."referenceId"
         and m."userId" = new."userId"
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     );
end;

create trigger "oauth_consent_requires_ready_member_update" before update on "oauthConsent"
begin
  select raise(abort, 'oauth.membership_invalid')
  where new."userId" is null
     or new."referenceId" is null
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = new."referenceId"
         and m."userId" = new."userId"
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     );
end;

create trigger "oauth_code_requires_ready_member_and_session" before insert on "verification"
when json_valid(new."value") and json_extract(new."value", '$.type') = 'authorization_code'
begin
  select raise(abort, 'oauth.membership_invalid')
  where json_extract(new."value", '$.userId') is null
     or json_extract(new."value", '$.referenceId') is null
     or json_extract(new."value", '$.sessionId') is null
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = json_extract(new."value", '$.referenceId')
         and m."userId" = json_extract(new."value", '$.userId')
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     )
     or not exists (
       select 1 from "session" s
       where s."id" = json_extract(new."value", '$.sessionId')
         and s."userId" = json_extract(new."value", '$.userId')
         and (case when typeof(s."expiresAt") in ('integer', 'real') then s."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(s."expiresAt") > julianday('now') end)
     );
end;

create trigger "oauth_refresh_requires_ready_member" before insert on "oauthRefreshToken"
begin
  select raise(abort, 'oauth.membership_invalid')
  where new."referenceId" is null
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = new."referenceId"
         and m."userId" = new."userId"
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     );
end;

create trigger "oauth_access_requires_ready_member" before insert on "oauthAccessToken"
when new."userId" is not null
begin
  select raise(abort, 'oauth.membership_invalid')
  where new."referenceId" is null
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = new."referenceId"
         and m."userId" = new."userId"
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     );
end;

create trigger "oauth_revoke_on_member_delete" after delete on "member"
begin
  delete from "oauthConsent"
  where "userId" = old."userId" and "referenceId" = old."organizationId";
  update "oauthRefreshToken"
  set "revoked" = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  where "userId" = old."userId" and "referenceId" = old."organizationId" and "revoked" is null;
  update "oauthAccessToken"
  set "revoked" = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  where "userId" = old."userId" and "referenceId" = old."organizationId" and "revoked" is null;
end;
