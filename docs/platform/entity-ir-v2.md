# EntityIR v2

EntityIR v2 is how an `entity` artifact declares a type of business object: its properties, links, keys, invariants and lifecycles. The wire types are in `crates/norn-ir/src/entity_v2/`. `norn-ir` only decodes; every consistency rule is a `norn-checker` diagnostic with a stable code and a JSON-pointer span.

## The payload

An `entity` payload with `"entityVersion": "2.0"` is the v2 form. It holds exactly one definition:

| Member | Definition |
|---|---|
| `objectType` | an object type (`ObjectTypeDef`) |
| `interface` | an interface (`InterfaceDef`) |
| `objectTypeExtension` | members added to an existing type (`ObjectTypeExtension`) |
| `propertyTypes` | a catalog of shared property types (`PropertyTypeDef`) |
| `markings` | a catalog of classification markings |

A payload without `entityVersion` is v1 (see [v1 to v2 lowering](#v1-to-v2-lowering)). A `relation` artifact has no v2 form: in v2 a relation is a link on its source type, and a `relation` payload with `entityVersion` is refused.

Per-artifact checks run in `validate_entity_ir_v2_payload` (`norn-checker/src/typed_roots.rs`). Rules across definitions run over the whole artifact set in `check_entity_model` (`norn-checker/src/entity_model.rs`).

Wire rules that apply everywhere: unknown members are refused, optional members may be omitted, and an explicit `null` is refused (#2819).

## Object types

An object type (`entity_v2/object.rs`) has:

- `name`: unique across object types and interfaces, in the artifact set and against the installed base layer. The dotted prefix is its domain namespace.
- `extends`: at most one supertype. See [Inheritance and narrowing](#inheritance-and-narrowing).
- `implements`: interfaces, in addition to those of its supertypes.
- `properties`, `links`, `ownedCollections`.
- `identity`: zero or more keys. A key is an ordered list of property ids (one for a single key, several for a composite key), unique among the type's live objects in one tenant scope and world, enforced at commit. Order matters. Every object also has its kernel-assigned id, so a business key is optional. An identity in an external system is provenance on ingested facts, not a key.
- `invariants`, `lifecycles`, `markings`.
- `status`: the maturity of the definition (omitted means `active`). This is not a state of the objects; that is a lifecycle.
- `immutable`: existing instances are append-only. Subtypes inherit it.

The per-type checks are in `norn-checker/src/object_types.rs`: property ids, keys, identity rules, defaults and computed properties.

## Properties and property types

A property (`Property`) has an `id`, a `type`, optional `constraints`, and either a literal `default` or a `computed` expression, never both. A computed property is read-only and cannot be a key component.

A property type is one of twelve kinds (`entity_v2/property.rs`):

| Kind | Values |
|---|---|
| `text` | Unicode string, optionally with a `format` |
| `integer` | signed 64-bit integer |
| `decimal` | exact decimal with a declared `scale` (at most 28), optionally a unit or a currency |
| `decimal-binding` | interface-only decimal whose unit and scale come from the implementor |
| `boolean` | `true` or `false` |
| `timestamp` | an instant, with a required timezone policy |
| `date` | ISO 8601 calendar date, no zone |
| `duration` | exact elapsed duration, fixed-length units only |
| `enum` | one of a listed set of values |
| `json-ext` | opaque JSON, optionally described by a JSON Schema |
| `record` | a closed structured value; every member is present |
| `ref` | a shared property type, by name |

There is no binary floating-point kind. Numeric bounds and integer or decimal defaults are exact decimal strings (`"0.00"`), never JSON numbers. Units are UCUM or QUDT codes. A reference to another object is a link, not a property.

`propertyTypes` names a type with its constraints (`commerce.sku_code`) so object types reuse it through `ref`. The catalog is checked on its own by `check_property_type_defs` (`norn-checker/src/property_types.rs`): name grammar, unique names, refs that resolve and do not cycle, unique enum values, bound grammar and order, which constraints apply to which kind, and that patterns compile.

Constraints (`PropertyConstraints`): `nullable` (default `false`), inclusive `min` and `max`, `minLength` and `maxLength` in Unicode scalar values, and `pattern` (an unanchored `regex-lite` match). A property's constraints add to those of its `ref`'d type. A default must meet every constraint set that applies to it.

## Links

A link (`entity_v2/link.rs`) is declared once, on its source type: an id (the forward role, a member of the source), a target object type or interface, a cardinality for both ends (`to`, `from`, each `one` or `many`, plus `required`), an optional `inverse` role (a member of the target, derived, never declared again on the target), an `acyclic` flag, and optional link properties.

## Inheritance and narrowing

`extends` names one object type, local or base, and the chain must not return to its start. A subtype inherits every property, key, link, interface, invariant and lifecycle of its supertype.

Redeclaring an inherited id is an override, and an override may only **narrow**: every value it admits, the inherited declaration must admit too (`narrowing` in `norn-checker/src/entity_model.rs`):

- A `ref N` requirement: the override's `ref` chain passes through `N` (it is `N` or a shared type built on it).
- An inline type: the override resolves to an equal type, except that an `enum` may keep a subset of the values, a `text` may add a `format`, and a `json-ext` may add a `schema`.
- Nullable only if the inherited property is nullable.
- The inherited constraints still apply; they are joined with the override's.
- No override involves a computed property (`object_type.validation.override_computed`).
- Against a `ref` requirement with a declared additivity, the override keeps it exactly.

Interfaces work the same way: each required id must be a property of the type (declared or inherited) whose type narrows the requirement. When several interfaces require one id, one property must meet all of them, or it is `object_type.validation.interface_requirement_conflict`.

Tenants and extensions cannot shadow base members. Base-layer ids never contain `__`. Members a tenant type or an extension adds are named `ns__name` in the layer's extension namespace. A tenant layer declares no interfaces, and its object types are subtypes named in the tenant namespace, so a tenant only narrows.

## Lifecycles

A lifecycle (`entity_v2/lifecycle.rs`) is the business state machine of one `enum` property, such as `status`. The states are the enum's values; the lifecycle does not restate them. A type may have several lifecycles, one per property.

A lifecycle declares `initial` states, `terminal` states and named transitions. A transition has an id, one or more `from` states, a `to` state and optional `guards`. Actions refer to the transition; the lifecycle does not name the action.

The checker (`norn-checker/src/entity_model/lifecycles.rs`) requires:

- The property is stored, not nullable, and resolves to a local `enum`.
- `initial` is not empty. Every named state is a value of the enum, with no repeats. The property's default, if any, is an initial state.
- Transition ids follow the member grammar and are unique. `from` is not empty and does not contain `to`. Each `(from, to)` pair is declared once. No transition leaves a terminal state. No guard is blank.
- Every state is reachable from an initial state, and every non-terminal state has an outgoing transition.
- A subtype inherits the machine, restricted to the states its enum keeps. A redeclared lifecycle may only drop transitions, drop source states and drop initial states, and must restate every inherited guard verbatim. It may append guards; guards are joined with AND. Dropping or replacing an inherited guard is `lifecycle.validation.drops_guard`.

Over the artifact set, `lifecycle_writes` (`norn-checker/src/lifecycle_writes.rs`, through `check_lifecycle_writes` in `norn-checker/src/lifecycles.rs`) refuses any decision or action that writes a lifecycle property other than by a declared transition.

## v1 to v2 lowering

EntityIR v1 (`objectKind` plus a `fields` map) is still accepted while artifacts migrate. Its bytes and hashes never change. Lowering is a read-side view (`norn-checker/src/entity_lowering.rs`): consumers read every `entity` artifact through `entity_object_type`, which decodes v2 directly and lowers v1 with `lower_entity_v1`.

| v1 | v2 |
|---|---|
| `objectKind` | `objectType.name` |
| `fields` | `properties`, in field-name order; the id is the field name, verbatim |
| `required: true` | `nullable` omitted (`false`) |
| `required` omitted or `false` | `nullable: true` |
| `text`, `string` | `text` |
| `integer` | `integer` |
| `number` | `json-ext` with schema `{"type": "number"}` |
| `boolean` | `boolean` |
| `object` | `json-ext` with schema `{"type": "object"}` |
| `array` | `json-ext` with schema `{"type": "array"}` |
| `enum` | `enum` |
| `mutationAuthority`, `extensions` | the same members of the v2 root |
| `overlaySources` | dropped |

A v1 `number` does not become a `decimal`: a decimal needs a scale, and any scale would refuse or round values v1 accepts. Author the v2 artifact with an exact `decimal` and the scale you need.

Names are kept verbatim, even when they break the v2 id grammar, because the field name is the key of stored payloads. The lowered view is not re-checked against v2 authoring rules.

A v1 `relation` artifact lowers to a link on its `from` type (`lower_relation_v1`). The link id is the relation artifact's envelope `name`, the target is `to`, there is no inverse, and the link is not required. The cardinality is read from `from` to `to` (`lower_cardinality_v1`):

| v1 `cardinality` | link `to` | link `from` |
|---|---|---|
| `one_to_one` | `one` | `one` |
| `one_to_many` | `many` | `one` |
| `many_to_one` | `one` | `many` |
| `many_to_many` | `many` | `many` |
| omitted | `many` | `many` |

An omitted v1 cardinality is many-to-many: the engine's existing default, and the only reading that constrains nothing.

Every plan change that adds or modifies a v1 `entity` artifact gets the warning `entity.deprecation.v1_payload`. Author new versions in v2.

## Expression sites

ZEN Expressions appear in three places in an EntityIR v2 model. Each reads the object through `self` (`self.net_weight * self.reorder_quantity`), never through bare property names. The expression language is `norn-expr`; see `crates/norn-expr/README.md`.

| Site | Where | What the checker does |
|---|---|---|
| Computed property | `properties[].computed` | Must not be blank (`object_type.validation.empty_computed`) and must parse (`object_type.validation.invalid_computed`). |
| Invariant | `invariants[].expression`, on object types and interfaces | Must parse, and must type-check as `bool` with `self` bound to the object type, or to the interface for an interface invariant (`entity_model/links.rs`). An invariant that reads a link or an inverse role is refused: cross-object constraints are not enforced yet (#3121). Both refusals are `object_type.validation.invalid_invariant_expression`. |
| Lifecycle guard | `lifecycles[].transitions[].guards[]` | Must not be blank (`lifecycle.validation.empty_guard`) and must parse (`lifecycle.validation.invalid_guard`) (`entity_model/lifecycles.rs`). At commit the kernel evaluates each guard; a guard that is false refuses the transition, and a guard that cannot be evaluated to a boolean refuses it fail-closed (`norn-kernel/src/decision_commit.rs`). |

The parse check is `check_zen_syntax` in `norn-checker/src/object_types.rs`, which calls `norn_expr::Expression::compile`. In the entity model check, only invariants are type-checked; computed properties and guards are parsed there, not typed.
