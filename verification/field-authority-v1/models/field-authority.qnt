// Finite reference contract, not a Rust refinement proof. Only evaluateRule
// mints a seal. Callers submit data through callerWrite, even when claiming rule.
module field_authority {
  type Result = { kind: str, supplies: bool, value: int, rule: int, pin: int }
  type Property = { computed: bool, protected: bool, lifecycle: bool }
  val empty = { kind: "none", supplies: false, value: 0, rule: 0, pin: 0 }
  var present: bool
  var stored: int
  var immutable: bool
  var protected: bool
  var beforeExists: bool
  var beforeValue: int
  var refused: bool
  var op: str
  var origin: str
  var seal: Result
  var lastTargetProtected: bool
  var lastSupplies: bool
  var lastAuthority: bool
  var lastRule: int
  var lastPin: int
  var checkerCas: bool
  var runtimeCas: bool

  action initial(im: bool, pr: bool): bool = all {
    present' = false, stored' = 0, immutable' = im, protected' = pr,
    beforeExists' = false, beforeValue' = 0, refused' = false,
    op' = "initial", origin' = "none", seal' = empty,
    lastTargetProtected' = false, lastSupplies' = false, lastAuthority' = false, lastRule' = 0, lastPin' = 0,
    checkerCas' = false, runtimeCas' = false,
  }
  action init = {
    nondet im = Set(false, true).oneOf()
    nondet pr = Set(false, true).oneOf()
    initial(im, pr)
  }
  // The registry admits rule 1 under definition pin 1. Evaluation produces
  // the exact candidate that commit consumes; no caller-provided output is sealed.
  action evaluateRule(rule: int, pin: int, kind: str, supplies: bool, value: int): bool = all {
    seal' = if (rule == 1 and pin == 1) { kind: kind, supplies: supplies, value: value, rule: rule, pin: pin } else empty,
    present' = present, stored' = stored, immutable' = immutable, protected' = protected,
    beforeExists' = present, beforeValue' = stored, refused' = false,
    op' = "evaluate", origin' = "kernel", lastTargetProtected' = false, lastSupplies' = false, lastAuthority' = false, lastRule' = 0, lastPin' = 0,
    checkerCas' = checkerCas, runtimeCas' = runtimeCas,
  }
  action apply(kind: str, producer: str, supplies: bool, nextValue: int, authority: bool, rule: int, pin: int, targetProtected: bool): bool = {
    val allowed = if (kind == "create") not(present) and (not(protected) or not(supplies) or authority)
      else present and not(immutable) and (not(if (kind == "set") targetProtected else protected) or authority)
    all {
      beforeExists' = present, beforeValue' = stored, immutable' = immutable, protected' = protected,
      refused' = not(allowed), op' = kind, origin' = producer,
      present' = if (allowed) kind != "delete" else present,
      stored' = if (allowed) (if (kind == "delete" or not(supplies)) 0 else nextValue) else stored,
      seal' = empty, lastTargetProtected' = targetProtected, lastSupplies' = supplies, lastAuthority' = authority, lastRule' = rule, lastPin' = pin,
      checkerCas' = checkerCas, runtimeCas' = runtimeCas,
    }
  }
  action callerWrite(kind: str, claim: str, supplies: bool, value: int, targetProtected: bool): bool =
    apply(kind, claim, supplies, value, false, 0, 0, targetProtected)
  action commitRule(rule: int, pin: int): bool = {
    val authority = seal.kind != "none" and seal.rule == rule and seal.pin == pin and rule == 1 and pin == 1
    if (authority) apply(seal.kind, "rule", seal.supplies, seal.value, true, rule, pin, protected)
    else all {
      present' = present, stored' = stored, immutable' = immutable, protected' = protected,
      beforeExists' = present, beforeValue' = stored, refused' = true, op' = "invalid_seal", origin' = "rule",
      seal' = empty, lastTargetProtected' = false, lastSupplies' = false, lastAuthority' = false, lastRule' = rule, lastPin' = pin,
      checkerCas' = checkerCas, runtimeCas' = runtimeCas,
    }
  }
  def writable(p: Property): bool = not(p.computed) and not(p.protected) and not(p.lifecycle)
  // Admission of the declared CAS touch, assuming a current, populated parent
  // snapshot. Missing values and version conflicts are separate runtime checks.
  action checkCas(properties: Set[Property]): bool = all {
    checkerCas' = properties.exists(p => writable(p)),
    runtimeCas' = properties.filter(p => not(p.computed) and not(p.protected) and not(p.lifecycle)).size() > 0,
    present' = present, stored' = stored, immutable' = immutable, protected' = protected,
    beforeExists' = present, beforeValue' = stored, refused' = false, op' = "cas", origin' = "action",
    seal' = seal, lastTargetProtected' = false, lastSupplies' = false, lastAuthority' = false, lastRule' = 0, lastPin' = 0,
  }
  def protectedWrite = (protected and ((op == "create" and lastSupplies) or (beforeExists and op == "delete"))) or (beforeExists and op == "set" and lastTargetProtected)
  def onlyDeclaredRulesOwnProtectedWrites = (protectedWrite and not(refused)) implies lastAuthority
  def ruleAuthorityIsKernelSealed = lastAuthority implies (origin == "rule" and lastRule == 1 and lastPin == 1)
  def callerCannotAssertRuleAuthority = (op != "initial" and origin == "rule" and not(lastAuthority) and protectedWrite) implies refused
  def checkerRuntimeCasAgree = checkerCas == runtimeCas
  def invariant = (refused implies (present == beforeExists and stored == beforeValue))
    and ((immutable and beforeExists) implies (present and stored == beforeValue))
    and onlyDeclaredRulesOwnProtectedWrites and ruleAuthorityIsKernelSealed
    and callerCannotAssertRuleAuthority and checkerRuntimeCasAgree
  action step = {
    nondet kind = Set("create", "set", "delete").oneOf()
    nondet target = Set(false, true).oneOf()
    nondet producer = Set("action", "rule", "observation", "owned_rows").oneOf()
    nondet supplies = Set(false, true).oneOf()
    nondet value = 0.to(2).oneOf()
    nondet rule = 0.to(2).oneOf()
    nondet pin = 0.to(2).oneOf()
    nondet c = Set(false, true).oneOf()
    nondet p = Set(false, true).oneOf()
    nondet l = Set(false, true).oneOf()
    any {
      callerWrite(kind, producer, supplies, value, protected and target),
      evaluateRule(rule, pin, kind, supplies, value),
      commitRule(rule, pin),
      checkCas(Set({ computed: c, protected: p, lifecycle: l })),
    }
  }
  run deleteThenRecreateRefusedTest = initial(false, true)
    .then(evaluateRule(1, 1, "create", true, 2)).then(commitRule(1, 1))
    .then(callerWrite("delete", "action", false, 0, protected))
    .then(all { assert(invariant and refused and present and stored == 2), callerWrite("create", "action", false, 0, protected) })
    .then(all { assert(invariant and refused and present and stored == 2), callerWrite("set", "rule", true, 1, protected) })
    .then(all { assert(invariant and refused and stored == 2), evaluateRule(0, 0, "none", false, 0) })
  run forgedOriginRefusedTest = initial(false, true)
    .then(callerWrite("create", "rule", true, 2, protected))
    .then(all { assert(invariant and refused and not(present)), commitRule(1, 1) })
    .then(all { assert(invariant and refused and not(present)), evaluateRule(0, 0, "none", false, 0) })
  run exactRuleBindingTest = initial(false, true)
    .then(evaluateRule(1, 1, "create", true, 1)).then(commitRule(2, 1))
    .then(all { assert(invariant and refused and not(present)), evaluateRule(1, 1, "create", true, 2) })
    .then(commitRule(1, 2))
    .then(all { assert(invariant and refused and not(present)), evaluateRule(1, 1, "create", true, 2) })
    .then(commitRule(1, 1)).then(all { assert(invariant and not(refused) and stored == 2), evaluateRule(0, 0, "none", false, 0) })
  run rowOnlyCasAdmissionTest = initial(false, false)
    .then(checkCas(Set({ computed: false, protected: true, lifecycle: false })))
    .then(all { assert(invariant and not(checkerCas)), checkCas(Set({ computed: true, protected: false, lifecycle: false })) })
    .then(all { assert(invariant and not(checkerCas)), checkCas(Set({ computed: false, protected: false, lifecycle: true })) })
    .then(all { assert(invariant and not(checkerCas)), checkCas(Set({ computed: false, protected: true, lifecycle: false }, { computed: false, protected: false, lifecycle: false })) })
    .then(all { assert(invariant and checkerCas), evaluateRule(0, 0, "none", false, 0) })
  run mixedProtectionAdmissionTest = initial(false, true)
    .then(evaluateRule(1, 1, "create", true, 2)).then(commitRule(1, 1))
    .then(callerWrite("set", "action", true, 1, false))
    .then(all { assert(invariant and not(refused) and stored == 1), callerWrite("set", "action", true, 0, true) })
    .then(all { assert(invariant and refused and stored == 1), callerWrite("delete", "action", false, 0, false) })
    .then(all { assert(invariant and refused and present), evaluateRule(0, 0, "none", false, 0) })

}
