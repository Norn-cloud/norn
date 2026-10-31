// Reference authority model. The host supplies an admitted rule origin;
// this does not prove caller authentication or Rust implementation correspondence.
module field_authority {
  var present: bool
  var stored: int
  var immutable: bool
  var protected: bool
  var beforeExists: bool
  var beforeValue: int
  var refused: bool
  var op: str
  var origin: str
  action initial(im: bool, pr: bool): bool = all {
    present' = false, stored' = 0, immutable' = im, protected' = pr,
    beforeExists' = false, beforeValue' = 0, refused' = false,
    op' = "initial", origin' = "none",
  }
  action init = {
    nondet im = Set(false, true).oneOf()
    nondet pr = Set(false, true).oneOf()
    initial(im, pr)
  }
  action write(kind: str, producer: str, supplies: bool, next: int): bool = {
    val allowed = if (kind == "create") not(present) and (not(protected) or not(supplies) or producer == "rule")
      else present and not(immutable) and (kind == "delete" or not(protected) or producer == "rule")
    all {
      beforeExists' = present, beforeValue' = stored, immutable' = immutable, protected' = protected,
      refused' = not(allowed), op' = kind, origin' = producer,
      present' = if (allowed) kind != "delete" else present,
      stored' = if (allowed) (if (kind == "delete" or not(supplies)) 0 else next) else stored,
    }
  }
  action step = {
    nondet kind = Set("create", "set", "delete").oneOf()
    nondet producer = Set("action", "rule", "observation", "owned_rows").oneOf()
    nondet supplies = Set(false, true).oneOf()
    nondet next = 0.to(2).oneOf()
    write(kind, producer, supplies, next)
  }
  def invariant = (refused implies (present == beforeExists and stored == beforeValue))
    and ((immutable and beforeExists) implies (present and stored == beforeValue))
    and ((protected and beforeExists and op == "set" and origin != "rule") implies refused)
  run ruleCanUpdateProtectedTest = initial(false, true)
    .then(write("create", "rule", true, 1))
    .then(write("set", "action", true, 2))
    .then(all { assert(invariant and refused and stored == 1), write("set", "rule", true, 2) })
    .then(all { assert(invariant and not(refused) and stored == 2), write("delete", "rule", true, 0) })
}
