module Core_models

// Minimal explicit model for the only hax core operation referenced by the
// selected extraction. No other Rust or hax core primitive is modeled here.
class negation_tc self = {
  ( ~. ): self -> self;
}

instance negation_for_bool: negation_tc bool = {
  ( ~. ) = not
}
