-- Commit-time OAuth membership-generation, session, and per-claim guards (S3):
-- Every OAuth grant reference encodes `<organizationId>|<memberId>` (an immutable membership
-- generation). Legacy bare-org grants without `|<memberId>` are refused by every trigger.
-- 1. At most one claimed consent approval row may exist per (userId, clientId, grantRef), even
--    after expiry, until atomically rolled back and deleted.
-- 2. oauthConsent INSERT/UPDATE requires BOTH an exact ready membership generation AND an active
--    claimed consent approval for that exact (userId, clientId, grantRef) bound to a live,
--    unexpired caller session (with a narrow rollback path restoring only the pre-request consent
--    snapshot recorded on the claimed approval).
-- 3. authorization_code INSERT in verification requires an exact ready membership generation,
--    a live unexpired session, a matching oauthConsent row for (userId, clientId, referenceId),
--    and—whenever ANY claimed approval exists for that triple (provisional consent)—MUST carry
--    that exact unexpired claim's `claimId` and match its sessionId, redirectUri, codeChallenge,
--    and state; on commit, it consumes ONLY that exact `claimId`.
-- 4. oauthRefreshToken and user oauthAccessToken INSERT require an exact ready membership generation.
-- 5. Deleting a membership immediately deletes matching consents & unredeemed authorization codes
--    and revokes matching refresh & access tokens so rejoin can never resurrect prior grants.

create trigger "oauth_single_claimed_approval_per_grant" before update on "verification"
when new."identifier" glob 'oauth-approval-claimed:*'
begin
  select raise(abort, 'oauth.approval_conflict')
  where not json_valid(new."value")
     or json_extract(new."value", '$.type') != 'consent_approval'
     or json_extract(new."value", '$.userId') is null
     or json_extract(new."value", '$.clientId') is null
     or json_extract(new."value", '$.grantRef') is null
     or json_extract(new."value", '$.sessionId') is null
     or json_extract(new."value", '$.sessionToken') is null
     or json_extract(new."value", '$.redirectUri') is null
     or exists (
       select 1 from "verification" v
       where v."id" != new."id"
         and v."identifier" glob 'oauth-approval-claimed:*'
         and json_valid(v."value")
         and json_extract(v."value", '$.type') = 'consent_approval'
         and json_extract(v."value", '$.userId') = json_extract(new."value", '$.userId')
         and json_extract(v."value", '$.clientId') = json_extract(new."value", '$.clientId')
         and json_extract(v."value", '$.grantRef') = json_extract(new."value", '$.grantRef')
     );
end;

create trigger "oauth_consent_requires_ready_member_insert" before insert on "oauthConsent"
begin
  select raise(abort, 'oauth.membership_invalid')
  where new."userId" is null
     or new."clientId" is null
     or new."referenceId" is null
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
       select 1 from "verification" v
       join "session" s on s."id" = json_extract(v."value", '$.sessionId')
         and s."token" = json_extract(v."value", '$.sessionToken')
         and s."userId" = new."userId"
         and (case when typeof(s."expiresAt") in ('integer', 'real') then s."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(s."expiresAt") > julianday('now') end)
       where v."identifier" glob 'oauth-approval-claimed:*'
         and json_valid(v."value")
         and json_extract(v."value", '$.type') = 'consent_approval'
         and json_extract(v."value", '$.userId') = new."userId"
         and json_extract(v."value", '$.clientId') = new."clientId"
         and json_extract(v."value", '$.grantRef') = new."referenceId"
         and (case when typeof(v."expiresAt") in ('integer', 'real') then v."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(v."expiresAt") > julianday('now') end)
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
    and exists (
      select 1 from "verification" v
      where v."identifier" glob 'oauth-approval-claimed:*'
        and json_valid(v."value")
        and json_extract(v."value", '$.type') = 'consent_approval'
        and json_extract(v."value", '$.userId') = old."userId"
        and json_extract(v."value", '$.clientId') = old."clientId"
        and json_extract(v."value", '$.grantRef') = old."referenceId"
        and json_extract(v."value", '$.prevConsent.id') = old."id"
        and new."scopes" = json_extract(v."value", '$.prevConsent.scopes')
        and (
          (new."resources" is null and json_extract(v."value", '$.prevConsent.resources') is null)
          or new."resources" = json_extract(v."value", '$.prevConsent.resources')
        )
        and (
          (new."requestedUserInfoClaims" is null and json_extract(v."value", '$.prevConsent.requestedUserInfoClaims') is null)
          or new."requestedUserInfoClaims" = json_extract(v."value", '$.prevConsent.requestedUserInfoClaims')
        )
    )
  )
  and (
    new."userId" is null
    or new."clientId" is null
    or new."referenceId" is null
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
      select 1 from "verification" v
      join "session" s on s."id" = json_extract(v."value", '$.sessionId')
        and s."token" = json_extract(v."value", '$.sessionToken')
        and s."userId" = new."userId"
        and (case when typeof(s."expiresAt") in ('integer', 'real') then s."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(s."expiresAt") > julianday('now') end)
      where v."identifier" glob 'oauth-approval-claimed:*'
        and json_valid(v."value")
        and json_extract(v."value", '$.type') = 'consent_approval'
        and json_extract(v."value", '$.userId') = new."userId"
        and json_extract(v."value", '$.clientId') = new."clientId"
        and json_extract(v."value", '$.grantRef') = new."referenceId"
        and (case when typeof(v."expiresAt") in ('integer', 'real') then v."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(v."expiresAt") > julianday('now') end)
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
     )
     or exists (
       select 1 from "verification" v
       where v."identifier" glob 'oauth-approval-claimed:*'
         and json_valid(v."value")
         and json_extract(v."value", '$.type') = 'consent_approval'
         and json_extract(v."value", '$.userId') = json_extract(new."value", '$.userId')
         and json_extract(v."value", '$.clientId') = json_extract(new."value", '$.query.client_id')
         and json_extract(v."value", '$.grantRef') = json_extract(new."value", '$.referenceId')
         and (
           json_extract(new."value", '$.claimId') is null
           or v."id" != json_extract(new."value", '$.claimId')
           or not (case when typeof(v."expiresAt") in ('integer', 'real') then v."expiresAt" > (julianday('now') - 2440587.5) * 86400000.0 else julianday(v."expiresAt") > julianday('now') end)
           or json_extract(v."value", '$.sessionId') != json_extract(new."value", '$.sessionId')
           or json_extract(v."value", '$.redirectUri') != coalesce(json_extract(new."value", '$.query.redirect_uri'), '')
           or coalesce(json_extract(v."value", '$.codeChallenge'), '') != coalesce(json_extract(new."value", '$.query.code_challenge'), '')
           or coalesce(json_extract(v."value", '$.state'), '') != coalesce(json_extract(new."value", '$.query.state'), '')
         )
     );
end;

create trigger "oauth_consume_claimed_approval_on_code" after insert on "verification"
when json_valid(new."value")
  and json_extract(new."value", '$.type') = 'authorization_code'
  and json_extract(new."value", '$.claimId') is not null
begin
  delete from "verification"
  where "id" = json_extract(new."value", '$.claimId')
    and "identifier" = 'oauth-approval-claimed:' || "id"
    and json_valid("value")
    and json_extract("value", '$.type') = 'consent_approval'
    and json_extract("value", '$.userId') = json_extract(new."value", '$.userId')
    and json_extract("value", '$.clientId') = json_extract(new."value", '$.query.client_id')
    and json_extract("value", '$.grantRef') = json_extract(new."value", '$.referenceId')
    and json_extract("value", '$.sessionId') = json_extract(new."value", '$.sessionId');
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
  where ("identifier" glob 'oauth-approval:*' or "identifier" glob 'oauth-approval-claimed:*')
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
