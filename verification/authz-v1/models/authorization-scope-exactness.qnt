module authorization_scope_exactness {
  // Optional dimensions are encoded as 0=None, 1=Some(A), 2=Some(B).
  // The finite request stream attempts every representation for the five
  // optional dimensions against one exact persisted grant.
  var grantTenant: int
  var grantWorkspace: int
  var grantEnvironment: int
  var grantBusinessUnit: int
  var grantSession: int
  var lastRequestTenant: int
  var lastRequestWorkspace: int
  var lastRequestEnvironment: int
  var lastRequestBusinessUnit: int
  var lastRequestSession: int
  var lastAllowed: bool

  action init = all {
    grantTenant' = 0,
    grantWorkspace' = 1,
    grantEnvironment' = 0,
    grantBusinessUnit' = 2,
    grantSession' = 0,
    lastRequestTenant' = 0,
    lastRequestWorkspace' = 1,
    lastRequestEnvironment' = 0,
    lastRequestBusinessUnit' = 2,
    lastRequestSession' = 0,
    lastAllowed' = true,
  }

  def exact(
      tenant: int,
      workspace: int,
      environment: int,
      businessUnit: int,
      session: int,
    ): bool = all {
    grantTenant == tenant,
    grantWorkspace == workspace,
    grantEnvironment == environment,
    grantBusinessUnit == businessUnit,
    grantSession == session,
  }

  action evaluate(
      tenant: int,
      workspace: int,
      environment: int,
      businessUnit: int,
      session: int,
    ): bool = all {
    lastRequestTenant' = tenant,
    lastRequestWorkspace' = workspace,
    lastRequestEnvironment' = environment,
    lastRequestBusinessUnit' = businessUnit,
    lastRequestSession' = session,
    lastAllowed' = exact(tenant, workspace, environment, businessUnit, session),
    grantTenant' = grantTenant,
    grantWorkspace' = grantWorkspace,
    grantEnvironment' = grantEnvironment,
    grantBusinessUnit' = grantBusinessUnit,
    grantSession' = grantSession,
  }

  action step = nondet tenant = 0.to(2).oneOf()
    nondet workspace = 0.to(2).oneOf()
    nondet environment = 0.to(2).oneOf()
    nondet businessUnit = 0.to(2).oneOf()
    nondet session = 0.to(2).oneOf()
    evaluate(tenant, workspace, environment, businessUnit, session)

  def typeOk = all {
    grantTenant >= 0, grantTenant <= 2,
    grantWorkspace >= 0, grantWorkspace <= 2,
    grantEnvironment >= 0, grantEnvironment <= 2,
    grantBusinessUnit >= 0, grantBusinessUnit <= 2,
    grantSession >= 0, grantSession <= 2,
    lastRequestTenant >= 0, lastRequestTenant <= 2,
    lastRequestWorkspace >= 0, lastRequestWorkspace <= 2,
    lastRequestEnvironment >= 0, lastRequestEnvironment <= 2,
    lastRequestBusinessUnit >= 0, lastRequestBusinessUnit <= 2,
    lastRequestSession >= 0, lastRequestSession <= 2,
  }

  def allowIffExact = lastAllowed == exact(
    lastRequestTenant,
    lastRequestWorkspace,
    lastRequestEnvironment,
    lastRequestBusinessUnit,
    lastRequestSession,
  )

  def omittedGrantTenantRejectsPresentRequest = not(all {
    grantTenant == 0,
    lastRequestTenant != 0,
    lastAllowed,
  })

  def omittedGrantSessionRejectsPresentRequest = not(all {
    grantSession == 0,
    lastRequestSession != 0,
    lastAllowed,
  })
}
