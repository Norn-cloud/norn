# Ascent 0.8.1: clock-free Norn profile

## Provenance and license

Source: https://github.com/s-arash/ascent at git commit
`e52c84b4419c56976413de1cbf15d56c8c136a5c`, as recorded in both
published `.cargo_vcs_info.json` files (retained verbatim).
The directories `ascent/` and `ascent_macro/` retain every file in the
corresponding official crates.io archives, including their original manifests
and package lockfiles. The normalized `Cargo.toml` files are authoritative;
`Cargo.toml.orig` and package lockfiles are untouched provenance, not Norn's
workspace dependency resolution.

| Package | Official archive | SHA-256 |
| --- | --- | --- |
| ascent 0.8.1 | https://static.crates.io/crates/ascent/ascent-0.8.1.crate | `4b69c5a0326edc45aea8469a51f84be0ff28ea1e3a4d087665468793be061682` |
| ascent_macro 0.8.1 | https://static.crates.io/crates/ascent_macro/ascent_macro-0.8.1.crate | `1ac2662b20bf37fd66d21687c0ef91f1c64320a7ca985a34d1c3e60cc9ba940a` |

Both packages are MIT licensed. The archives omit the license file; `LICENSE`
is copied verbatim from
https://raw.githubusercontent.com/s-arash/ascent/e52c84b4419c56976413de1cbf15d56c8c136a5c/LICENSE
(SHA-256 `351332fea36f5ad5247c57d0e4afe9630ed4337d456de32e7d9a78f5c1255dbd`). Preserve its copyright and permission notice.

`ascent_base` is deliberately not vendored: its code needs no patch. The
0.8.1 archive was audited for clock uses and depends only on `pastey`:
https://static.crates.io/crates/ascent_base/ascent_base-0.8.1.crate,
SHA-256 `792af0babe403e49368d8717763605fe978b3a1eb7d51b52e3d0a5cb97e6eb74`.

## Profile and feature unification

A Norn consumer's workspace edge is exactly:

```toml
ascent = { version = "=0.8.1", default-features = false, features = ["no-clock"] }
```

with a root `[patch.crates-io]` that redirects only `ascent` and `ascent_macro`
to these sources (added together with the first consumer). Both packages are
explicitly excluded from workspace membership; no default members, dependency
guards, or CI admission rules are changed by vendoring them. The runtime
pins its patched macro dependency to `=0.8.1` and forwards `no-clock` to it.

Upstream default behavior is retained through defaults `par` and `clock`.
The new `clock` feature enables the now-optional `web-time` dependency. An
upstream-style sequential consumer disabling defaults must explicitly select
`clock`. Norn selects `no-clock`, which enables neither `clock` nor `par`.
Cargo features unify additively: every edge to Ascent in the same feature
resolution must disable defaults and omit `clock` for the Norn profile.
Selecting both profiles, or neither, produces an explicit compile error;
there is no fake clock and no silent fallback. Macro features also unify:
consumers must not mix clocked runtime code with a directly enabled
`ascent_macro/no-clock` feature.

In `no-clock`, sequential and parallel index moves perform the same index
operations but omit all clock reads and timing accumulation. The runtime's
`Instant` re-export and global duration counters are absent. The experimental
`exps` test/benchmark module is excluded, including its `std::time::Instant`
reads. The macro omits SCC/index timing reads, timing fields and timing
summary method, and emits no timeout method. `measure_rule_times` and
`generate_run_timeout` attributes are explicitly rejected with a spanned
error. Deterministic SCC iteration counting remains. Outside `no-clock`,
index algorithms, timing, timeout generation and rule timing retain upstream
behavior. Some adjacent formatting follows direct rustfmt of touched files;
pristine vendored Rust files are not formatted.

Only this Ascent feature subgraph excludes `web-time`; the whole Norn
workspace already contains unrelated `web-time` edges. No Norn crate consumes
Ascent in this change: the checker integration is the next change. Manifest
inspection is not a resolved dependency-graph claim. The integrating change
must run standard Cargo resolution (never manually edit Cargo.lock), then
inspect the actual consumer's normal dependency tree and prove that its Ascent
subtree excludes `web-time` and `clock`.

## Guards and qualification

`ascent_macro/src/tests.rs` contains `no_clock_` tests using the existing
private `ascent_impl` seam, without new dependencies or scratchpad IO. They
parse small sequential/parallel and normal/run expansions and reject
`Instant`, `web_time`, timeout methods and timing fields; separate cases
require both timing attributes to return errors.

These tests require off-host execution. From the repository root:

```sh
cargo test --manifest-path third_party/ascent/ascent_macro/Cargo.toml --features no-clock no_clock_
```

A standalone runtime check needs the macro patch explicitly, because these
packages are excluded from Norn's workspace (root patches do not apply when
an excluded package is used as its own Cargo root). On the off-host checkout,
with its absolute checkout path substituted:

```sh
cargo check --manifest-path third_party/ascent/ascent/Cargo.toml --no-default-features --features no-clock --config 'patch.crates-io.ascent_macro.path="/ABSOLUTE_CHECKOUT/third_party/ascent/ascent_macro"'
```

Also qualify the same runtime command with `--features no-clock,par`, and the
default clocked profile, plus the workspace's relevant gates. Isolated macro
unit tests resolve `ascent_base` normally.

Qualification record (Rust 1.98.0, on a disposable Linux VM): the macro
`no_clock_` tests pass (2 passed); the runtime `cargo check` passes for
`no-clock`, `no-clock,par` and the default clocked profile, and fails with the
explicit `select either clock or no-clock` error when neither is selected. CI
admission and lock resolution remain integration gates, not exceptions added by
this patch.

Static integration preflight: relation columns require `Clone + Eq + Hash`;
lattice columns additionally require `Lattice`, and parallel columns require
`Send + Sync` (`ascent/src/internal.rs`). Existing Norn `facts::Value`,
`facts::Predicate`, and `facts::Fact` lack `Hash` at this base. The next slice
must choose compatible typed columns or establish the needed bounds; this
vendor patch changes no Norn types or inference semantics. Macro/runtime
imports and existing dependency declarations are retained, with only
`web-time` made optional and the macro's feature propagated.

## Exact local source diff

The following zero-context unified diff (apply with `git apply --unidiff-zero`) is relative to the two extracted official archives,
with paths rooted at this directory. It covers every modified archive file.
All other archive files are byte-identical. The additional files are
this note, the verbatim upstream `LICENSE`, and `.gitattributes` preserving
the two upstream CRLF files against repository text normalization. Root workspace wiring is
reviewed in the repository's `Cargo.toml` diff.

```diff
--- a/ascent/Cargo.toml
+++ b/ascent/Cargo.toml
@@ -38 +38,3 @@
-default = ["par"]
+default = ["par", "clock"]
+clock = ["dep:web-time"]
+no-clock = ["ascent_macro/no-clock"]
@@ -132 +134 @@
-version = "0.8.1"
+version = "=0.8.1"
@@ -167,0 +170 @@
+optional = true
--- a/ascent/src/c_lat_index.rs
+++ b/ascent/src/c_lat_index.rs
@@ -6,0 +7 @@
+#[cfg(not(feature = "no-clock"))]
@@ -84 +85,3 @@
-   pub fn hash_usize(&self, k: &K) -> usize { self.unwrap_unfrozen().hash_usize(k) }
+   pub fn hash_usize(&self, k: &K) -> usize {
+      self.unwrap_unfrozen().hash_usize(k)
+   }
@@ -106,3 +109,7 @@
-   fn len_estimate(&self) -> usize { self.unwrap_frozen().len() }
-
-   fn is_empty(&'a self) -> bool { self.unwrap_frozen().len() == 0 }
+   fn len_estimate(&self) -> usize {
+      self.unwrap_frozen().len()
+   }
+
+   fn is_empty(&'a self) -> bool {
+      self.unwrap_frozen().len() == 0
+   }
@@ -147,0 +155 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -179,0 +188 @@
+      #[cfg(not(feature = "no-clock"))]
--- a/ascent/src/c_rel_full_index.rs
+++ b/ascent/src/c_rel_full_index.rs
@@ -5,0 +6 @@
+#[cfg(not(feature = "no-clock"))]
@@ -75,3 +76,7 @@
-   fn insert(&self, key: K, value: V) { self.unwrap_unfrozen().insert(key, value); }
-
-   pub fn hash_usize(&self, k: &K) -> usize { self.unwrap_unfrozen().hash_usize(k) }
+   fn insert(&self, key: K, value: V) {
+      self.unwrap_unfrozen().insert(key, value);
+   }
+
+   pub fn hash_usize(&self, k: &K) -> usize {
+      self.unwrap_unfrozen().hash_usize(k)
+   }
@@ -80 +85,3 @@
-   where V: Clone {
+   where
+      V: Clone,
+   {
@@ -125,3 +132,7 @@
-   fn len_estimate(&self) -> usize { self.unwrap_frozen().len() }
-
-   fn is_empty(&'a self) -> bool { self.unwrap_frozen().len() == 0 }
+   fn len_estimate(&self) -> usize {
+      self.unwrap_frozen().len()
+   }
+
+   fn is_empty(&'a self) -> bool {
+      self.unwrap_frozen().len() == 0
+   }
@@ -148 +159,3 @@
-   fn contains_key(&self, key: &Self::Key) -> bool { self.unwrap_frozen().contains_key(key) }
+   fn contains_key(&self, key: &Self::Key) -> bool {
+      self.unwrap_frozen().contains_key(key)
+   }
@@ -248,0 +262 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -268,0 +283 @@
+      #[cfg(not(feature = "no-clock"))]
--- a/ascent/src/c_rel_index.rs
+++ b/ascent/src/c_rel_index.rs
@@ -8,0 +9 @@
+#[cfg(not(feature = "no-clock"))]
@@ -107 +108,3 @@
-   pub fn hash_usize(&self, k: &K) -> usize { self.unwrap_unfrozen().hash_usize(k) }
+   pub fn hash_usize(&self, k: &K) -> usize {
+      self.unwrap_unfrozen().hash_usize(k)
+   }
@@ -178,0 +182 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -208,0 +213 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -233 +238,3 @@
-   fn clone(&self) -> Self { Self { shards: self.shards } }
+   fn clone(&self) -> Self {
+      Self { shards: self.shards }
+   }
@@ -237 +244,3 @@
-   pub fn new(v: &'a ReadOnlyView<K, V, S>) -> Self { Self { shards: v.shards() } }
+   pub fn new(v: &'a ReadOnlyView<K, V, S>) -> Self {
+      Self { shards: v.shards() }
+   }
@@ -250 +259,3 @@
-   where C: UnindexedConsumer<Self::Item> {
+   where
+      C: UnindexedConsumer<Self::Item>,
+   {
@@ -277 +288,3 @@
-   where C: UnindexedConsumer<Self::Item> {
+   where
+      C: UnindexedConsumer<Self::Item>,
+   {
--- a/ascent/src/c_rel_no_index.rs
+++ b/ascent/src/c_rel_no_index.rs
@@ -2,0 +3 @@
+#[cfg(not(feature = "no-clock"))]
@@ -32 +33,3 @@
-   pub fn hash_usize(&self, _key: &()) -> usize { 0 }
+   pub fn hash_usize(&self, _key: &()) -> usize {
+      0
+   }
@@ -36,2 +39,6 @@
-   fn freeze(&mut self) { self.frozen = true; }
-   fn unfreeze(&mut self) { self.frozen = false; }
+   fn freeze(&mut self) {
+      self.frozen = true;
+   }
+   fn unfreeze(&mut self) {
+      self.frozen = false;
+   }
@@ -60 +67,3 @@
-   fn len_estimate(&self) -> usize { 1 }
+   fn len_estimate(&self) -> usize {
+      1
+   }
@@ -62 +71,3 @@
-   fn is_empty(&'a self) -> bool { false }
+   fn is_empty(&'a self) -> bool {
+      false
+   }
@@ -95,0 +107 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -110,0 +123 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -136 +149,3 @@
-   fn iter_all(&'a self) -> Self::AllIteratorType { std::iter::once((&(), self.index_get(&()).unwrap())) }
+   fn iter_all(&'a self) -> Self::AllIteratorType {
+      std::iter::once((&(), self.index_get(&()).unwrap()))
+   }
@@ -147 +162,3 @@
-   fn c_iter_all(&'a self) -> Self::AllIteratorType { rayon::iter::once((&(), self.c_index_get(&()).unwrap())) }
+   fn c_iter_all(&'a self) -> Self::AllIteratorType {
+      rayon::iter::once((&(), self.c_index_get(&()).unwrap()))
+   }
--- a/ascent/src/internal.rs
+++ b/ascent/src/internal.rs
@@ -4,0 +5 @@
+#[cfg(not(feature = "no-clock"))]
@@ -9,0 +11 @@
+#[cfg(not(feature = "no-clock"))]
@@ -91,0 +94 @@
+#[cfg(not(feature = "no-clock"))]
@@ -92,0 +96 @@
+#[cfg(not(feature = "no-clock"))]
@@ -117,0 +122 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -136,0 +142 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -147 +153,3 @@
-   fn index_insert(&mut self, _key: Self::Key, tuple_index: usize) { self.push(tuple_index); }
+   fn index_insert(&mut self, _key: Self::Key, tuple_index: usize) {
+      self.push(tuple_index);
+   }
@@ -151 +159,3 @@
-   fn move_index_contents(ind1: &mut Self, ind2: &mut Self) { ind2.append(ind1); }
+   fn move_index_contents(ind1: &mut Self, ind2: &mut Self) {
+      ind2.append(ind1);
+   }
@@ -159 +169,3 @@
-   fn index_insert(&mut self, key: Self::Key, tuple_index: V) { self.entry(key).or_default().insert(tuple_index); }
+   fn index_insert(&mut self, key: Self::Key, tuple_index: V) {
+      self.entry(key).or_default().insert(tuple_index);
+   }
@@ -171,0 +184 @@
+#[cfg(not(feature = "no-clock"))]
@@ -172,0 +186 @@
+#[cfg(not(feature = "no-clock"))]
@@ -180 +194,3 @@
-   fn index_insert(&mut self, key: Self::Key, value: V) { self.insert(key, value); }
+   fn index_insert(&mut self, key: Self::Key, value: V) {
+      self.insert(key, value);
+   }
@@ -184,0 +201 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -192,0 +210 @@
+      #[cfg(not(feature = "no-clock"))]
@@ -217 +235,3 @@
-   fn contains_key(&self, key: &Self::Key) -> bool { self.contains_key(key) }
+   fn contains_key(&self, key: &Self::Key) -> bool {
+      self.contains_key(key)
+   }
@@ -222 +242,2 @@
-where T: Clone + Eq + Hash
+where
+   T: Clone + Eq + Hash,
@@ -228 +249,2 @@
-where T: Clone + Eq + Hash + Lattice
+where
+   T: Clone + Eq + Hash + Lattice,
@@ -235 +257,2 @@
-where T: Send + Sync
+where
+   T: Send + Sync,
--- a/ascent/src/lib.rs
+++ b/ascent/src/lib.rs
@@ -5,0 +6,5 @@
+
+#[cfg(all(feature = "clock", feature = "no-clock"))]
+compile_error!("ascent: no-clock requires default-features = false and no clock feature in the unified graph");
+#[cfg(not(any(feature = "clock", feature = "no-clock")))]
+compile_error!("ascent: select either clock or no-clock");
@@ -11,0 +17 @@
+#[cfg(not(feature = "no-clock"))]
--- a/ascent_macro/Cargo.toml
+++ b/ascent_macro/Cargo.toml
@@ -26,0 +27,3 @@
+
+[features]
+no-clock = []
--- a/ascent_macro/src/ascent_codegen.rs
+++ b/ascent_macro/src/ascent_codegen.rs
@@ -78,0 +79,8 @@
+      let (scc_start, scc_end) = if cfg!(feature = "no-clock") {
+         (quote! {}, quote! {})
+      } else {
+         (
+            quote! { let _scc_start_time = ::ascent::internal::Instant::now(); },
+            quote! { _self.scc_times[#i] += _scc_start_time.elapsed(); },
+         )
+      };
@@ -82 +90 @@
-            let _scc_start_time = ::ascent::internal::Instant::now();
+            #scc_start
@@ -84 +92 @@
-            _self.scc_times[#i] += _scc_start_time.elapsed();
+            #scc_end
@@ -169 +177 @@
-   let generate_run_timeout = !is_ascent_run && mir.config.generate_run_partial;
+   let generate_run_timeout = !cfg!(feature = "no-clock") && !is_ascent_run && mir.config.generate_run_partial;
@@ -266,0 +275,24 @@
+   let (clock_fields, clock_defaults, update_start, update_end, times_summary) = if cfg!(feature = "no-clock") {
+      (quote! {}, quote! {}, quote! {}, quote! {}, quote! {})
+   } else {
+      (
+         quote! {
+            scc_times: [std::time::Duration; #sccs_count],
+            update_time_nanos: std::sync::atomic::AtomicU64,
+            update_indices_duration: std::time::Duration,
+         },
+         quote! {
+            scc_times: [std::time::Duration::ZERO; #sccs_count],
+            update_time_nanos: Default::default(),
+            update_indices_duration: std::time::Duration::default(),
+         },
+         quote! { let before = ::ascent::internal::Instant::now(); },
+         quote! { self.update_indices_duration += before.elapsed(); },
+         quote! {
+            pub fn scc_times_summary(&self) -> String {
+               #![allow(clippy::all)]
+               #scc_times_summary_body
+            }
+         },
+      )
+   };
@@ -273 +305 @@
-         scc_times: [std::time::Duration; #sccs_count],
+         #clock_fields
@@ -276,2 +307,0 @@
-         update_time_nanos: std::sync::atomic::AtomicU64,
-         update_indices_duration: std::time::Duration,
@@ -287 +317 @@
-            let before = ::ascent::internal::Instant::now();
+            #update_start
@@ -289 +319 @@
-            self.update_indices_duration += before.elapsed();
+            #update_end
@@ -306,4 +336 @@
-         pub fn scc_times_summary(&self) -> String {
-            #![allow(clippy::all)]
-            #scc_times_summary_body
-         }
+         #times_summary
@@ -315 +342 @@
-               scc_times: [std::time::Duration::ZERO; #sccs_count],
+               #clock_defaults
@@ -318,2 +344,0 @@
-               update_time_nanos: Default::default(),
-               update_indices_duration: std::time::Duration::default()
@@ -551 +576 @@
-      let (before_rule_var, update_rule_time_field) = if mir.config.include_rule_times {
+      let (before_rule_var, update_rule_time_field) = if !cfg!(feature = "no-clock") && mir.config.include_rule_times {
@@ -1128 +1153 @@
-               continue
+               continue;
@@ -1401,3 +1426,6 @@
-            assignments.insert(0, quote! {
-               let __row = &#relation_expr[*#val_ident]#maybe_lock #maybe_clone;
-            });
+            assignments.insert(
+               0,
+               quote! {
+                  let __row = &#relation_expr[*#val_ident]#maybe_lock #maybe_clone;
+               },
+            );
@@ -1406,3 +1434,6 @@
-            assignments.insert(0, quote! {
-               let #val_ident = #val_ident.tuple_of_borrowed();
-            });
+            assignments.insert(
+               0,
+               quote! {
+                  let #val_ident = #val_ident.tuple_of_borrowed();
+               },
+            );
--- a/ascent_macro/src/ascent_hir.rs
+++ b/ascent_macro/src/ascent_hir.rs
@@ -45,0 +46,8 @@
+      #[cfg(feature = "no-clock")]
+      for attr in &attrs {
+         if attr.meta.path().is_ident(Self::MEASURE_RULE_TIMES_ATTR)
+            || attr.meta.path().is_ident(Self::GENERATE_RUN_TIMEOUT_ATTR)
+         {
+            return Err(Error::new_spanned(attr, "timing attributes are unavailable with ascent's no-clock feature"));
+         }
+      }
@@ -162 +170,3 @@
-   pub fn selected_args(&self) -> Vec<Expr> { self.rel.indices.iter().map(|&i| self.args[i].clone()).collect() }
+   pub fn selected_args(&self) -> Vec<Expr> {
+      self.rel.indices.iter().map(|&i| self.args[i].clone()).collect()
+   }
@@ -205,3 +215,9 @@
-   pub fn ir_name(&self) -> Ident { ir_name_for_rel_indices(&self.relation.name, &self.indices) }
-   pub fn is_full_index(&self) -> bool { self.relation.field_types.len() == self.indices.len() }
-   pub fn is_no_index(&self) -> bool { self.indices.is_empty() }
+   pub fn ir_name(&self) -> Ident {
+      ir_name_for_rel_indices(&self.relation.name, &self.indices)
+   }
+   pub fn is_full_index(&self) -> bool {
+      self.relation.field_types.len() == self.indices.len()
+   }
+   pub fn is_no_index(&self) -> bool {
+      self.indices.is_empty()
+   }
@@ -260,2 +276,3 @@
-         (Some(attr), true) =>
-            return Err(Error::new(attr.path.span(), "`lattice`s cannot have custom data structure providers")),
+         (Some(attr), true) => {
+            return Err(Error::new(attr.path.span(), "`lattice`s cannot have custom data structure providers"));
+         },
@@ -265,13 +282,20 @@
-      relations_metadata.insert(rel_identity.clone(), RelationMetadata {
-         initialization: rel.initialization.clone().map(Rc::new),
-         attributes: Rc::new(
-            rel.attrs
-               .iter()
-               .filter(|attr| {
-                  attr.meta.path().get_ident().map_or(true, |ident| !RECOGNIIZED_REL_ATTRS.iter().any(|ra| ident == ra))
-               })
-               .cloned()
-               .collect_vec(),
-         ),
-         ds_attr,
-      });
+      relations_metadata.insert(
+         rel_identity.clone(),
+         RelationMetadata {
+            initialization: rel.initialization.clone().map(Rc::new),
+            attributes: Rc::new(
+               rel.attrs
+                  .iter()
+                  .filter(|attr| {
+                     attr
+                        .meta
+                        .path()
+                        .get_ident()
+                        .map_or(true, |ident| !RECOGNIIZED_REL_ATTRS.iter().any(|ra| ident == ra))
+                  })
+                  .cloned()
+                  .collect_vec(),
+            ),
+            ds_attr,
+         },
+      );
@@ -513 +537 @@
-      Some(rel) =>
+      Some(rel) => {
@@ -525 +549,2 @@
-         },
+         }
+      },
--- a/ascent_macro/src/tests.rs
+++ b/ascent_macro/src/tests.rs
@@ -14 +14,3 @@
-   fn from(value: AscentMacroKind) -> Self { Self::Ascent(value) }
+   fn from(value: AscentMacroKind) -> Self {
+      Self::Ascent(value)
+   }
@@ -57 +58,0 @@
-
@@ -402 +403,3 @@
-            pub fn size(&self) -> i32 { self.x * self.x + self.y * self.y }
+            pub fn size(&self) -> i32 {
+               self.x * self.x + self.y * self.y
+            }
@@ -530,0 +534,42 @@
+
+#[cfg(feature = "no-clock")]
+#[test]
+fn no_clock_expansion_has_no_ambient_clock_or_timeout() {
+   for is_parallel in [false, true] {
+      for is_ascent_run in [false, true] {
+         let input = quote! {
+            relation edge(i32, i32);
+            relation path(i32, i32);
+            edge(1, 2);
+            path(x, y) <-- edge(x, y);
+            path(x, z) <-- path(x, y), edge(y, z);
+         };
+         let code = ascent_impl(input, AscentMacroKind { is_ascent_run, is_parallel }).unwrap();
+         if is_ascent_run {
+            syn::parse2::<syn::Expr>(code.clone()).unwrap();
+         } else {
+            syn::parse2::<syn::File>(code.clone()).unwrap();
+         }
+         let expanded = code.to_string();
+         for forbidden in
+            ["Instant", "web_time", "run_timeout", "scc_times", "update_indices_duration", "update_time_nanos"]
+         {
+            assert!(!expanded.contains(forbidden), "unexpected {forbidden} in expansion: {expanded}");
+         }
+      }
+   }
+}
+
+#[cfg(feature = "no-clock")]
+#[test]
+fn no_clock_rejects_timing_attributes() {
+   for attribute in [quote! { #![measure_rule_times] }, quote! { #![generate_run_timeout] }] {
+      let input = quote! {
+         #attribute
+         relation value(i32);
+         value(1);
+      };
+      let error = ascent_impl(input, AscentMacroKind::default()).unwrap_err();
+      assert!(error.to_string().contains("timing attributes are unavailable"));
+   }
+}
```
