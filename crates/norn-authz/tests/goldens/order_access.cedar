@id("view_own_or_watched")
permit (
  principal is Norn::Actor,
  action in [Norn::Action::"commerce.order.view"],
  resource is commerce::SalesOrder
)
when {
  principal.scope == context.scope &&
  resource.scope == context.scope &&
  context.scope has tenant &&
  (resource has "watchers" && principal in resource["watchers"])
};

@id("approve")
permit (
  principal is Norn::Actor,
  action in [Norn::Action::"commerce.order.approve"],
  resource is commerce::SalesOrder
)
when {
  principal.scope == context.scope &&
  resource.scope == context.scope &&
  context.scope has tenant &&
  context.scope has environment &&
  principal in [Norn::Role::"approver", Norn::Role::"finance.manager"] &&
  (context has "mfa" && context["mfa"] == true) &&
  (context has "channel" && context["channel"] != "api \"legacy\"")
};

@id("view_any_in_org")
permit (
  principal is Norn::Actor,
  action in [Norn::Action::"commerce.order.view", Norn::Action::"commerce.order.approve"],
  resource is commerce::SalesOrder
)
when {
  principal.scope == context.scope &&
  resource.scope == context.scope &&
  principal in [Norn::Role::"auditor"]
};
