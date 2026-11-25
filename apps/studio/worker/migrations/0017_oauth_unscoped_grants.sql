-- Assistant grants are workspace-scoped (#1703). A consent, refresh token or access token recorded before
-- grants carried `<organizationId>|<memberId>` names no workspace: the server can no longer resolve it
-- without guessing one, so it is withdrawn and the person consents again for a workspace they choose.
-- Migration 0013's triggers already refuse writing new unscoped rows.
DELETE FROM "oauthConsent"
WHERE "referenceId" IS NULL
   OR instr("referenceId", '|') <= 1
   OR instr(substr("referenceId", instr("referenceId", '|') + 1), '|') > 0
   OR length(substr("referenceId", instr("referenceId", '|') + 1)) = 0;
UPDATE "oauthRefreshToken" SET "revoked" = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE "revoked" IS NULL AND ("referenceId" IS NULL
   OR instr("referenceId", '|') <= 1
   OR instr(substr("referenceId", instr("referenceId", '|') + 1), '|') > 0
   OR length(substr("referenceId", instr("referenceId", '|') + 1)) = 0);
UPDATE "oauthAccessToken" SET "revoked" = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE "revoked" IS NULL AND "userId" IS NOT NULL AND ("referenceId" IS NULL
   OR instr("referenceId", '|') <= 1
   OR instr(substr("referenceId", instr("referenceId", '|') + 1), '|') > 0
   OR length(substr("referenceId", instr("referenceId", '|') + 1)) = 0);
