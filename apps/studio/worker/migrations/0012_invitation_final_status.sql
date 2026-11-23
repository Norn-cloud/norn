-- An invitation's answer is final. Only a pending invitation changes status: accepting, rejecting and
-- canceling each move it out of 'pending', and nothing moves it again. Better Auth's reject and cancel
-- routes read the invitation and then write its status in a second statement, so an acceptance that
-- commits between the two would otherwise be overwritten (an "accepted" invitation turned "rejected"
-- while its member stays). The guard is at the write, for every writer: Studio's own accept
-- (workspaces.ts) and Better Auth's routes alike. The Worker answers the refusal as
-- INVITATION_NOT_FOUND (index.ts guarded).
CREATE TRIGGER "invitation_status_final" BEFORE UPDATE OF "status" ON "invitation"
  WHEN old."status" <> 'pending' AND new."status" IS NOT old."status"
BEGIN
  SELECT RAISE(ABORT, 'invitation.not_pending');
END;
