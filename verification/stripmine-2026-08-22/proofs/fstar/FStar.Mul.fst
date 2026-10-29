module FStar.Mul

// hax currently emits this open even when the selected pure function does not
// use multiplication. The pinned F* distribution no longer ships that legacy
// namespace, so the extraction lane supplies an explicit empty compatibility
// module rather than admitting an axiom.
