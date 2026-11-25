-- Commit-time OAuth membership-generation, session, and exact per-claim guards (S3):
-- Every OAuth grant reference encodes `<organizationId>|<memberId>` (an immutable membership
-- generation). Legacy bare-org grants without `|<memberId>` are refused by every trigger.
-- 1. Claimed consent approvals live in the dedicated `oauth_consent_claim` table (outside
--    Better Auth's `verification` expiry sweep) with a unique index on (userId, clientId,
--    referenceId) so at most one claimed approval exists per grant until atomically consumed
--    by authorization-code issuance or atomically rolled back and deleted.
-- 2. oauthConsent carries `claimId` and its INSERT/UPDATE triggers require BOTH an exact ready
--    membership generation AND that the exact live unexpired `oauth_consent_claim` row
--    `cl."id" = new."claimId"` exists for (userId, clientId, referenceId) bound to a live
--    unexpired caller session (with a narrow rollback path restoring only the pre-request
--    consent snapshot recorded on `cl."id" = old."claimId"`).
-- 3. authorization_code INSERT in verification requires an exact ready membership generation,
--    a live unexpired session, a matching oauthConsent row whose committed `scopes`, `resources`,
--    and `requestedUserInfoClaims` cover every scope/resource/claim requested in the code, AND:
--    - when `$.claimId` IS supplied (consent-accept flow), that EXACT live unexpired claim row
--      MUST exist in `oauth_consent_claim`, match (userId, clientId, referenceId, sessionId,
--      redirectUri, codeChallenge, state), AND match `c."claimId"`;
--    - when `$.claimId` is NULL (silent re-authorize flow), NO claim row may exist for
--      (userId, clientId, referenceId);
--    on commit, it consumes ONLY that exact `claimId`.
-- 4. oauthRefreshToken and user oauthAccessToken INSERT require an exact ready membership generation.
-- 5. Deleting a membership immediately deletes matching consents, active claims, & unredeemed
--    authorization codes and revokes matching refresh & access tokens so rejoin can never
--    resurrect prior grants.

alter table "oauthConsent" add column "claimId" text;

create table "oauth_consent_claim" (
  "id" text not null primary key,
  "userId" text not null,
  "clientId" text not null,
  "referenceId" text not null,
  "sessionId" text not null,
  "sessionToken" text not null,
  "redirectUri" text not null,
  "codeChallenge" text,
  "state" text,
  "prevConsent" text,
  "expiresAt" text not null,
  "createdAt" text not null
);

create unique index "oauth_consent_claim_grant_uidx" on "oauth_consent_claim" ("userId", "clientId", "referenceId");

create trigger "oauth_consent_requires_ready_member_insert" before insert on "oauthConsent"
begin
  select raise(abort, 'oauth.membership_invalid')
  where new."userId" is null
     or new."clientId" is null
     or new."referenceId" is null
     or new."claimId" is null
     or instr(new."referenceId", '|') <= 1
     or instr(substr(new."referenceId", instr(new."referenceId", '|') + 1), '|') > 0
     or length(substr(new."referenceId", instr(new."referenceId", '|') + 1)) = 0
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = substr(new."referenceId", 1, instr(new."referenceId", '|') - 1)
         and m."id" = substr(new."referenceId", instr(new."referenceId", '|') + 1)
         and m."userId" = new."userId"
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     )
     or not exists (
       select 1 from "oauth_consent_claim" cl
       join "session" s on s."id" = cl."sessionId"
         and s."token" = cl."sessionToken"
         and s."userId" = new."userId"
         and (case when typeof(s."expiresAt") in ('integer', 'real') then s."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(s."expiresAt") > julianday('now') end)
       where cl."id" = new."claimId"
         and cl."userId" = new."userId"
         and cl."clientId" = new."clientId"
         and cl."referenceId" = new."referenceId"
         and (case when typeof(cl."expiresAt") in ('integer', 'real') then cl."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(cl."expiresAt") > julianday('now') end)
     );
end;

create trigger "oauth_consent_requires_ready_member_update" before update on "oauthConsent"
begin
  select raise(abort, 'oauth.membership_invalid')
  where not (
    new."id" = old."id"
    and new."clientId" = old."clientId"
    and new."userId" = old."userId"
    and new."referenceId" = old."referenceId"
    and old."claimId" is not null
    and exists (
      select 1 from "oauth_consent_claim" cl
      where cl."id" = old."claimId"
        and cl."userId" = old."userId"
        and cl."clientId" = old."clientId"
        and cl."referenceId" = old."referenceId"
        and cl."prevConsent" is not null
        and json_valid(cl."prevConsent")
        and json_extract(cl."prevConsent", '$.id') = old."id"
        and new."scopes" = json_extract(cl."prevConsent", '$.scopes')
        and (
          (new."resources" is null and json_extract(cl."prevConsent", '$.resources') is null)
          or new."resources" = json_extract(cl."prevConsent", '$.resources')
        )
        and (
          (new."requestedUserInfoClaims" is null and json_extract(cl."prevConsent", '$.requestedUserInfoClaims') is null)
          or new."requestedUserInfoClaims" = json_extract(cl."prevConsent", '$.requestedUserInfoClaims')
        )
        and (
          (new."claimId" is null and json_extract(cl."prevConsent", '$.claimId') is null)
          or new."claimId" = json_extract(cl."prevConsent", '$.claimId')
        )
    )
  )
  and (
    new."userId" is null
    or new."clientId" is null
    or new."referenceId" is null
    or new."claimId" is null
    or instr(new."referenceId", '|') <= 1
    or instr(substr(new."referenceId", instr(new."referenceId", '|') + 1), '|') > 0
    or length(substr(new."referenceId", instr(new."referenceId", '|') + 1)) = 0
    or not exists (
      select 1 from "member" m
      join "organization" o on o."id" = m."organizationId"
      where m."organizationId" = substr(new."referenceId", 1, instr(new."referenceId", '|') - 1)
        and m."id" = substr(new."referenceId", instr(new."referenceId", '|') + 1)
        and m."userId" = new."userId"
        and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
    )
    or not exists (
      select 1 from "oauth_consent_claim" cl
      join "session" s on s."id" = cl."sessionId"
        and s."token" = cl."sessionToken"
        and s."userId" = new."userId"
        and (case when typeof(s."expiresAt") in ('integer', 'real') then s."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(s."expiresAt") > julianday('now') end)
      where cl."id" = new."claimId"
        and cl."userId" = new."userId"
        and cl."clientId" = new."clientId"
        and cl."referenceId" = new."referenceId"
        and (case when typeof(cl."expiresAt") in ('integer', 'real') then cl."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(cl."expiresAt") > julianday('now') end)
    )
  );
end;

create trigger "oauth_code_requires_ready_member_and_session" before insert on "verification"
when json_valid(new."value") and json_extract(new."value", '$.type') = 'authorization_code'
begin
  select raise(abort, 'oauth.membership_invalid')
  where json_extract(new."value", '$.userId') is null
     or json_extract(new."value", '$.referenceId') is null
     or json_extract(new."value", '$.sessionId') is null
     or json_extract(new."value", '$.query.client_id') is null
     or instr(json_extract(new."value", '$.referenceId'), '|') <= 1
     or instr(substr(json_extract(new."value", '$.referenceId'), instr(json_extract(new."value", '$.referenceId'), '|') + 1), '|') > 0
     or length(substr(json_extract(new."value", '$.referenceId'), instr(json_extract(new."value", '$.referenceId'), '|') + 1)) = 0
     or instr(coalesce(json_extract(new."value", '$.query.scope'), ''), char(34)) > 0
     or instr(coalesce(json_extract(new."value", '$.query.scope'), ''), char(92)) > 0
     or instr(coalesce(json_extract(new."value", '$.query.scope'), ''), char(9)) > 0
     or instr(coalesce(json_extract(new."value", '$.query.scope'), ''), char(10)) > 0
     or instr(coalesce(json_extract(new."value", '$.query.scope'), ''), char(13)) > 0
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = substr(json_extract(new."value", '$.referenceId'), 1, instr(json_extract(new."value", '$.referenceId'), '|') - 1)
         and m."id" = substr(json_extract(new."value", '$.referenceId'), instr(json_extract(new."value", '$.referenceId'), '|') + 1)
         and m."userId" = json_extract(new."value", '$.userId')
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     )
     or not exists (
       select 1 from "session" s
       where s."id" = json_extract(new."value", '$.sessionId')
         and s."userId" = json_extract(new."value", '$.userId')
         and (case when typeof(s."expiresAt") in ('integer', 'real') then s."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(s."expiresAt") > julianday('now') end)
     )
     or not exists (
       select 1 from "oauthConsent" c
       where c."userId" = json_extract(new."value", '$.userId')
         and c."clientId" = json_extract(new."value", '$.query.client_id')
         and c."referenceId" = json_extract(new."value", '$.referenceId')
         and (
           json_extract(new."value", '$.claimId') is null
           or c."claimId" = json_extract(new."value", '$.claimId')
         )
         and json_valid(c."scopes")
         and json_type(c."scopes") = 'array'
         and not exists (
           select 1
           from json_each(
             case
               when trim(coalesce(json_extract(new."value", '$.query.scope'), '')) = '' then '[]'
               else json('["' || replace(trim(json_extract(new."value", '$.query.scope')), ' ', '","') || '"]')
             end
           ) req_scope
           where req_scope."value" != ''
             and not exists (
               select 1 from json_each(c."scopes") granted_scope
               where granted_scope."value" = req_scope."value"
             )
         )
         and (
           json_extract(new."value", '$.resource') is null
           or (
             json_type(new."value", '$.resource') = 'array'
             and not exists (
               select 1 from json_each(json_extract(new."value", '$.resource')) req_res
               where not (
                 c."resources" is not null
                 and json_valid(c."resources")
                 and json_type(c."resources") = 'array'
                 and exists (
                   select 1 from json_each(c."resources") granted_res
                   where granted_res."value" = req_res."value"
                 )
               )
             )
           )
         )
         and (
           json_extract(new."value", '$.query.claims') is null
           or (
             case
               when json_type(new."value", '$.query.claims') = 'object' then json_extract(new."value", '$.query.claims.userinfo')
               when json_type(new."value", '$.query.claims') = 'text' and json_valid(json_extract(new."value", '$.query.claims')) then json_extract(json_extract(new."value", '$.query.claims'), '$.userinfo')
               else null
             end is null
           )
           or (
             json_type(
               case
                 when json_type(new."value", '$.query.claims') = 'object' then json_extract(new."value", '$.query.claims.userinfo')
                 else json_extract(json_extract(new."value", '$.query.claims'), '$.userinfo')
               end
             ) = 'object'
             and not exists (
               select 1
               from json_each(
                 case
                   when json_type(new."value", '$.query.claims') = 'object' then json_extract(new."value", '$.query.claims.userinfo')
                   else json_extract(json_extract(new."value", '$.query.claims'), '$.userinfo')
                 end
               ) req_claim
               where req_claim."key" in ('name', 'picture', 'given_name', 'family_name', 'email', 'email_verified')
                 and not (
                 c."requestedUserInfoClaims" is not null
                 and json_valid(c."requestedUserInfoClaims")
                 and json_type(c."requestedUserInfoClaims") = 'array'
                 and exists (
                   select 1 from json_each(c."requestedUserInfoClaims") granted_claim
                   where granted_claim."value" = req_claim."key"
                 )
               )
             )
           )
         )
     )
     or (
       json_extract(new."value", '$.claimId') is not null
       and not exists (
         select 1 from "oauth_consent_claim" cl
         where cl."id" = json_extract(new."value", '$.claimId')
           and cl."userId" = json_extract(new."value", '$.userId')
           and cl."clientId" = json_extract(new."value", '$.query.client_id')
           and cl."referenceId" = json_extract(new."value", '$.referenceId')
           and cl."sessionId" = json_extract(new."value", '$.sessionId')
           and cl."redirectUri" = coalesce(json_extract(new."value", '$.query.redirect_uri'), '')
           and coalesce(cl."codeChallenge", '') = coalesce(json_extract(new."value", '$.query.code_challenge'), '')
           and coalesce(cl."state", '') = coalesce(json_extract(new."value", '$.query.state'), '')
           and (case when typeof(cl."expiresAt") in ('integer', 'real') then cl."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(cl."expiresAt") > julianday('now') end)
       )
     )
     or (
       json_extract(new."value", '$.claimId') is null
       and exists (
         select 1 from "oauth_consent_claim" cl
         where cl."userId" = json_extract(new."value", '$.userId')
           and cl."clientId" = json_extract(new."value", '$.query.client_id')
           and cl."referenceId" = json_extract(new."value", '$.referenceId')
       )
     );
end;

create trigger "oauth_consume_claimed_approval_on_code" after insert on "verification"
when json_valid(new."value")
  and json_extract(new."value", '$.type') = 'authorization_code'
  and json_extract(new."value", '$.claimId') is not null
begin
  delete from "oauth_consent_claim"
  where "id" = json_extract(new."value", '$.claimId')
    and "userId" = json_extract(new."value", '$.userId')
    and "clientId" = json_extract(new."value", '$.query.client_id')
    and "referenceId" = json_extract(new."value", '$.referenceId')
    and "sessionId" = json_extract(new."value", '$.sessionId');
end;

create trigger "oauth_refresh_requires_ready_member" before insert on "oauthRefreshToken"
begin
  select raise(abort, 'oauth.membership_invalid')
  where new."referenceId" is null
     or instr(new."referenceId", '|') <= 1
     or instr(substr(new."referenceId", instr(new."referenceId", '|') + 1), '|') > 0
     or length(substr(new."referenceId", instr(new."referenceId", '|') + 1)) = 0
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = substr(new."referenceId", 1, instr(new."referenceId", '|') - 1)
         and m."id" = substr(new."referenceId", instr(new."referenceId", '|') + 1)
         and m."userId" = new."userId"
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     );
end;

create trigger "oauth_access_requires_ready_member" before insert on "oauthAccessToken"
when new."userId" is not null
begin
  select raise(abort, 'oauth.membership_invalid')
  where new."referenceId" is null
     or instr(new."referenceId", '|') <= 1
     or instr(substr(new."referenceId", instr(new."referenceId", '|') + 1), '|') > 0
     or length(substr(new."referenceId", instr(new."referenceId", '|') + 1)) = 0
     or not exists (
       select 1 from "member" m
       join "organization" o on o."id" = m."organizationId"
       where m."organizationId" = substr(new."referenceId", 1, instr(new."referenceId", '|') - 1)
         and m."id" = substr(new."referenceId", instr(new."referenceId", '|') + 1)
         and m."userId" = new."userId"
         and (o."metadata" is null or not json_valid(o."metadata") or coalesce(json_extract(o."metadata", '$."norn.provisioning"'), 0) != 1)
     );
end;

create trigger "oauth_revoke_on_member_delete" after delete on "member"
begin
  delete from "oauth_consent_claim"
  where "userId" = old."userId"
    and ("referenceId" = old."organizationId" || '|' || old."id" or "referenceId" = old."organizationId");
  delete from "oauthConsent"
  where "userId" = old."userId"
    and ("referenceId" = old."organizationId" || '|' || old."id" or "referenceId" = old."organizationId");
  delete from "verification"
  where json_valid("value")
    and json_extract("value", '$.type') = 'authorization_code'
    and json_extract("value", '$.userId') = old."userId"
    and (json_extract("value", '$.referenceId') = old."organizationId" || '|' || old."id"
      or json_extract("value", '$.referenceId') = old."organizationId");
  delete from "verification"
  where "identifier" glob 'oauth-approval:*'
    and json_valid("value")
    and json_extract("value", '$.type') = 'consent_approval'
    and json_extract("value", '$.userId') = old."userId"
    and json_extract("value", '$.memberId') = old."id";
  update "oauthRefreshToken"
  set "revoked" = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  where "userId" = old."userId"
    and ("referenceId" = old."organizationId" || '|' || old."id" or "referenceId" = old."organizationId")
    and "revoked" is null;
  update "oauthAccessToken"
  set "revoked" = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  where "userId" = old."userId"
    and ("referenceId" = old."organizationId" || '|' || old."id" or "referenceId" = old."organizationId")
    and "revoked" is null;
end;
