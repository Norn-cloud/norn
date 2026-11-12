/**
 * Type-only placeholder surface for the Norn intrinsics the `norn-effect/1`
 * profile recognises (ruling 2026-09-27 authoring-effect-profile §1).
 *
 * This file declares types only. It is not the data builder (T7-FLOW-1) and
 * not the intrinsic builders (T7-TS-2); those replace these declarations with
 * the real closure-free builder package under the same module specifier. The
 * profile checker recognises these symbols by their resolved declaration
 * (`@norn/authoring#Norn.<name>`), never by spelling. The syntax compiler
 * lowers every call from its syntax; none of these functions ever runs.
 */
declare module "@norn/authoring" {
  import type { Effect, Schema } from "effect"

  export namespace Norn {
    /** Generated, versioned Function handle (compute-boundary ruling §2). */
    interface FnRef<I, O, E> {
      readonly _tag: "NornFnRef"
      readonly id: string
      readonly version: string
      readonly __types?: { readonly input: I; readonly output: O; readonly failure: E }
    }

    interface FnMeta {
      readonly id: string
      readonly version: string
    }

    interface FlowMeta {
      readonly id: string
      readonly version: string
    }

    /** Opaque flow definition value; the syntax compiler replaces the marker. */
    interface FlowDefinition<A, E> {
      readonly _tag: "NornFlowMarker"
      readonly __types?: { readonly result: A; readonly failure: E }
    }

    /** Declare a Function handle. Generic arguments carry its I/O/failure types. */
    function fn<I, O, E = never>(meta: FnMeta): FnRef<I, O, E>

    /** Durable step: command + waiting facts; accepted result selects the next transition. */
    function perform<I, O, E>(stepId: string, fn: FnRef<I, O, E>, input: I): Effect.Effect<O, E>

    /** A flow definition; the body is syntax consumed by the compiler, never executed. */
    function flow<A, E>(meta: FlowMeta, body: Effect.Effect<A, E>): FlowDefinition<A, E>

    // ------------------------------------------------ ontology (T7-EFX-4, T7-TS-2)

    /** A unit of measure in a standard vocabulary (UCUM `kg`, QUDT `unit:KiloGM`). */
    interface UnitRef {
      readonly system: "ucum" | "qudt"
      readonly code: string
    }

    type PropertyAdditivity =
      | { readonly kind: "undeclared" | "additive" | "semi_additive" | "non_additive" }
      | { readonly kind: "ratio"; readonly numerator: string; readonly denominator: string }

    /** Interface-only Decimal; members sharing a group require identical concrete unit and scale. */
    function boundDecimal(group: string): Schema.Finite

    /**
     * Exact decimal value type (EntityIR `decimal`): every value is held at
     * `scale` fraction digits, optionally a quantity in `unit`, or money whose
     * authoritative currency is the named sibling property (`{ currency }`,
     * EntityIR `currencySource`; required by metric SUM). Typed as a
     * number for authoring only; on the wire it is an exact number token and
     * never a binary float. `scale` is a non-negative integer literal.
     */
    function decimal(scale: number, options?: ({ readonly unit?: UnitRef } | { readonly currency: string }) & { readonly additivity?: PropertyAdditivity; readonly rounding?: "refuse" | "toward_zero" | "half_even" | "half_away_from_zero" }): Schema.Finite

    /** VAL-RATE: a typed currency-per-unit decimal; general compound units are not admitted. */
    function rate(scale: number, options: { readonly currency: string; readonly perUnit: UnitRef; readonly rounding?: "refuse" | "toward_zero" | "half_even" | "half_away_from_zero" }): Schema.Finite

    /**
     * A shared property type by name (EntityIR `ref`): a Norn Core registry
     * value type such as `iso4217.currency_code` or `iso3166.alpha2`, or a
     * local shared type. The checker resolves the name. `additivity` declares
     * the aggregation contract at this use site (METRIC-ADDITIVITY-REF-1): it
     * applies only to an integer or decimal type that declares none itself,
     * so a flow and a stock can share one value type.
     */
    function ref<S extends Schema.Top = Schema.String>(name: string, options?: { readonly additivity?: PropertyAdditivity }): S

    /** An instant with an explicit timezone policy (EntityIR `timestamp`; RFC 3339 text). */
    function timestamp(timezone: "utc" | "offset"): Schema.String

    /** A calendar date with no time and no zone (EntityIR `date`; `YYYY-MM-DD`). */
    function date(): Schema.String

    /** One declared transition of a lifecycle over the states `V`. */
    interface TransitionSpec<V extends string> {
      readonly id: string
      readonly description?: string
      readonly from: ReadonlyArray<V>
      readonly to: V
      /** ZEN Expression guards, conjoined. */
      readonly guards?: ReadonlyArray<Expression<boolean>>
      readonly guardRefusalCode?: string
    }

    /** A lifecycle over one enum property of `T`; its states are that property's values. */
    type LifecycleSpec<T> = {
      readonly [P in keyof T & string]: {
        readonly property: P
        readonly description?: string
        readonly initial: ReadonlyArray<Extract<T[P], string>>
        readonly terminal?: ReadonlyArray<Extract<T[P], string>>
        readonly immutable?: ReadonlyArray<Extract<T[P], string>>
        readonly transitions: ReadonlyArray<TransitionSpec<Extract<T[P], string>>>
      }
    }[keyof T & string]

    /** One business key: an ordered list of property ids. */
    interface KeySpec<T> {
      readonly name: string
      readonly properties: ReadonlyArray<keyof T & string>
    }

    /** A boolean ZEN integrity constraint or quality expectation on implementations. */
    interface InvariantSpec {
      readonly id: string
      readonly description?: string
      readonly classification?: "integrity" | "quality"
      readonly expression: Expression<boolean>
      readonly message: string
    }

    /** Opaque interface definition; the syntax compiler lowers the call. */
    interface InterfaceDefinition<T> {
      readonly _tag: "NornInterface"
      readonly __types?: { readonly type: T }
    }

    /** Opaque object type definition; the syntax compiler lowers the call. */
    interface TypeDefinition<T, L extends ReadonlyArray<LinkSpec> = ReadonlyArray<LinkSpec>> {
      readonly _tag: "NornType"
      readonly __types?: { readonly type: T; readonly links?: L }
    }

    /** Candidate QuestionIR; the host must admit it with the selected metric definitions. */
    interface QuestionDefinition { readonly _tag: "NornQuestion" }
    interface QuestionMeta {
      readonly name: string
      readonly of: string | TypeDefinition<unknown>
      readonly where: ReadonlyArray<{ readonly property: string; readonly operator: "eq" | "ne"; readonly value: string | number | boolean }>
      readonly measure: string
      readonly by: ReadonlyArray<{ readonly property: string; readonly grain?: "day" | "week" | "month" | "quarter" | "year" }>
      readonly asOf:
        | { readonly basis: "effective"; readonly from: string; readonly to: string; readonly freshness: { readonly kind: "live" | "delayed"; readonly maxAgeMs: number } }
        | { readonly basis: "knowledge" }
      readonly population: { readonly kind: "authorized_objects" }
    }
    /** Syntax only: authored modules and callbacks never execute. */
    function question(meta: QuestionMeta): QuestionDefinition

    /** Package declarations can augment these maps with registry property types. */
    interface RegistryObjectTypes {}
    interface RegistryInterfaceTypes {}

    type RegistryObject<Name extends string> = Name extends keyof RegistryObjectTypes ? RegistryObjectTypes[Name] : {}
    type RegistryInterface<Name extends string> = Name extends keyof RegistryInterfaceTypes ? RegistryInterfaceTypes[Name] : {}
    type ParentObject<P> = [P] extends [undefined]
      ? {}
      : P extends TypeDefinition<infer T> ? T : P extends string ? RegistryObject<P> : {}
    type InterfaceProperties<I> = I extends InterfaceDefinition<infer T> ? T : I extends string ? RegistryInterface<I> : {}
    type UnionToIntersection<U> = [U] extends [never]
      ? {}
      : (U extends unknown ? (value: U) => void : never) extends (value: infer I) => void ? I : {}
    type ImplementedProperties<I extends ReadonlyArray<unknown>> = UnionToIntersection<InterfaceProperties<I[number]>>
    type MergeProperties<Base, Own> = Own extends object ? Omit<Base, keyof Own> & Own : Own
    type EffectiveProperties<E, I extends ReadonlyArray<unknown>, Own> = MergeProperties<ParentObject<E> & ImplementedProperties<I>, Own>

    interface MarkingSpec {
      readonly id: string
      readonly label: string
      readonly severity: "low" | "moderate" | "high" | "critical"
      readonly category: string
      readonly mask: { readonly kind: "redacted" | "null" } | { readonly kind: "last-n"; readonly count: number }
    }
    /** Canonical EntityIR v2 catalog; values are masked by runtime lenses. */
    function markings(meta: { readonly id: string; readonly title: string; readonly version: string; readonly markings: ReadonlyArray<MarkingSpec> }): { readonly _tag: "NornMarkings" }

    /**
     * Localized labels (LABEL-LOCALE-1): BCP 47 tag → text. They lower to a
     * separate `<artifact id>.labels` artifact, never into the definition, so
     * they change no hash or evidence identity.
     */
    type LocaleText = import("../src/builder/labels.ts").LocaleText
    interface Localized {
      /** Short name per locale (`{ en: "Invoice", ar: "فاتورة" }`). */
      readonly label?: LocaleText
      /** Longer description per locale; `description` stays the canonical text. */
      readonly localizedDescription?: LocaleText
      /** Locales whose labels were proposed (for example machine-translated) and still need review. */
      readonly needsReview?: ReadonlyArray<string>
    }

    interface InterfaceMeta<S extends Schema.Top> extends Localized {
      /** Property labels, keyed by a declared property. */
      readonly propertyLabels?: { readonly [P in keyof S["Type"]]?: LocaleText }
      /** Enum value labels: declared enum property → stored value → label. */
      readonly valueLabels?: { readonly [P in keyof S["Type"]]?: { readonly [value: string]: LocaleText } }
      readonly role?: "thing" | "happening"
      readonly dependencies?: ReadonlyArray<import("../src/builder/envelope.ts").DependencyPin>
      /** Classification inherited by all properties. */
      readonly markings?: ReadonlyArray<string>
      /** Canonical property markings, keyed by a declared property. */
      readonly propertyMarkings?: { readonly [P in keyof S["Type"]]?: ReadonlyArray<string> }

      /** Qualified name (`commerce.Order`). */
      readonly name: string
      /** Human-readable title. */
      readonly title: string
      readonly version: string
      readonly description?: string
      /** The properties, as an Effect Schema struct (structural subset only). */
      readonly properties: S
    }

    /** A link every implementing type must declare, at most this wide (EntityIR `InterfaceLink`). */
    interface InterfaceLinkSpec {
      readonly id: string
      readonly description?: string
      /** The widest target: an object type or an interface, by reference or qualified name. */
      readonly target: TypeDefinition<unknown> | InterfaceDefinition<unknown> | string
      readonly cardinality: { readonly to: "one" | "many"; readonly from: "one" | "many"; readonly required?: boolean }
    }

    interface InterfaceInvariantMeta {
      /** Invariants over the required interface members, read through `self`. */
      readonly invariants?: ReadonlyArray<InvariantSpec>
    }

    /** A link to another object type (EntityIR v2 relations are links on the source type). */
    interface LinkSpec {
      readonly acyclic?: boolean
      readonly id: string
      readonly description?: string
      /** The target type, by reference or (for installed base types) by qualified name. */
      readonly target: TypeDefinition<unknown> | string
      readonly cardinality: { readonly to: "one" | "many"; readonly from: "one" | "many"; readonly required?: boolean }
      readonly inverse?: string
    }

    interface TypeMeta<
      S extends Schema.Top,
      E extends TypeDefinition<unknown> | string | undefined = undefined,
      I extends ReadonlyArray<InterfaceDefinition<unknown> | string> = readonly [],
      L extends ReadonlyArray<LinkSpec> = ReadonlyArray<LinkSpec>,
    > extends InterfaceMeta<S> {
      /** The one supertype, declared locally or named from the installed registry. */
      readonly extends?: E
      readonly implements?: I
      readonly pathEquations?: ReadonlyArray<import("../src/builder/entity.ts").PathEquation>
      readonly links?: L
      readonly identity?: ReadonlyArray<KeySpec<EffectiveProperties<E, I, S["Type"]>>>
      /** Literal defaults; numbers are exact decimal literals. */
      readonly defaults?: { readonly [P in keyof S["Type"]]?: S["Type"][P] }
      readonly lifecycles?: ReadonlyArray<LifecycleSpec<EffectiveProperties<E, I, S["Type"]>>>
    }

    /** An EntityIR v2 object type. */
    function type<
      S extends Schema.Top,
      const E extends TypeDefinition<unknown> | string | undefined = undefined,
      const I extends ReadonlyArray<InterfaceDefinition<unknown> | string> = readonly [],
      const L extends ReadonlyArray<LinkSpec> = readonly [],
    >(
      meta: TypeMeta<S, E, I, L>,
    ): TypeDefinition<EffectiveProperties<E, I, S["Type"]>, L>

    /** An EntityIR v2 interface (`interface` is a reserved word). */
    function interfaceType<
      S extends Schema.Top,
      const P extends ReadonlyArray<InterfaceDefinition<unknown> | string> = readonly [],
    >(
      meta: InterfaceMeta<S> & InterfaceInvariantMeta & { readonly extends?: P; readonly links?: ReadonlyArray<InterfaceLinkSpec> },
    ): InterfaceDefinition<EffectiveProperties<undefined, P, S["Type"]>>

    /** Opaque action definition; the syntax compiler lowers the call. */
    interface ActionDefinition<I> {
      readonly _tag: "NornAction"
      readonly __types?: { readonly input: I }
    }

    /** `ActionEffectProgram`: a closed, declarative effect on objects of `type`. */
    type ActionEffectSpec<I> =
      | {
          readonly kind: "fire_transition"
          readonly type: TypeDefinition<unknown>
          readonly objectIdField: keyof I & string
          readonly lifecycle: string
          readonly transition: string
          readonly acceptReceipt?: {
            readonly referenceField: keyof I & string
            readonly manifestDigest: string
            readonly digestProperty: string
            readonly resultBindings: Readonly<Record<string, string>>
          }
        }
      | {
          readonly kind: "upsert_input_object"
          readonly type: TypeDefinition<unknown>
          readonly objectIdField: keyof I & string
          readonly writeMode?:
            | { readonly kind: "upsert" }
            | { readonly kind: "create_only" }
            | { readonly kind: "draft_only"; readonly lifecycle: string; readonly state: string }
        }

    interface ActionMeta<I extends Schema.Top, O extends Schema.Top> extends Localized {
      readonly dependencies?: ReadonlyArray<import("../src/builder/envelope.ts").DependencyPin>
      /** `actionId` (`commerce.sales_order.approve`). */
      readonly id: string
      readonly title: string
      readonly version: string
      readonly description?: string
      readonly scopeRequired: ReadonlyArray<string>
      readonly idempotent: boolean
      readonly ledgered: boolean
      readonly ledgerCategory?: string
      /** Input and output: Effect Schema structs, lowered to closed schemas. */
      readonly input: I
      readonly output: O
      /**
       * Display-only labels for top-level input fields (D66): a title of 1 to
       * 120 characters and an optional description. They change no admission
       * semantics; forms show them.
       */
      readonly inputLabels?: { readonly [field in keyof I["Type"] & string]?: { readonly title?: string; readonly description?: string } }
      /**
       * Wizard steps over the input (D66): each field (an RFC 6901 pointer
       * such as `/valuation_method`) in at most one group.
       */
      readonly inputGroups?: ReadonlyArray<{ readonly id: string; readonly title: string; readonly fields: ReadonlyArray<string> }>
      /** Ordered bounded reads, with property keys rooted at `from: "input"`. */
      readonly slice?: ReadonlyArray<SliceSpec>
      /** Ordered deterministic ZEN transformations before validation and preconditions. */
      readonly derive?: ReadonlyArray<{
        readonly field: keyof I["Type"] & string
        readonly expression: Expression<unknown>
        /** Caller must supply this field; defaults to true. */
        readonly inputRequired?: boolean
      }>
      /** Boolean ZEN Expressions over `input` and the declared `slice`. */
      readonly preconditions?: ReadonlyArray<Expression<boolean>>
      /** Atomic typed links to captured or proposed objects; explicit sources are written by the closure. */
      readonly createLinks?: ReadonlyArray<{
        readonly source?: { readonly objectType: TypeDefinition<unknown> | string; readonly idField: keyof I["Type"] & string }
        readonly link: string
        readonly targetType: TypeDefinition<unknown> | string
        readonly targetIdField: keyof I["Type"] & string
      }>
      /** Connector commands the action enqueues (ERPNext write-back through perform); `name` and `fields` values are ZEN Expressions. */
      readonly commands?: ReadonlyArray<{
        readonly connector: string
        readonly operation: "submit" | "create" | "update"
        readonly doctype: string
        readonly name?: Expression<string>
        readonly fields?: { readonly [field: string]: Expression<unknown> }
      }>
      /** Pinned facts-only synchronous RuleIR writer; execution requires the matching host profile. */
      readonly synchronousRule?: DecisionDefinition | { readonly artifactId: string; readonly version: string }
      readonly writes?: ReadonlyArray<{ readonly type: TypeDefinition<unknown> | string; readonly change: "created" | "modified" | "deleted" }>
      readonly effect?: ActionEffectSpec<I["Type"]>
      readonly compensation?: ActionDefinition<unknown>
      /**
       * The access policy that authorizes calls (ActionIR `access`). A policy whose
       * rules name this action cannot be referenced here (the definitions would be
       * circular): give its artifact id, `<policy id>.policy`.
       */
      readonly access?: AccessDefinition | { readonly artifactId: string; readonly version?: string }
    }

    /** An ActionIR action. */
    function action<I extends Schema.Top, O extends Schema.Top>(meta: ActionMeta<I, O>): ActionDefinition<I["Type"]>

    type ScopeDimension = "org" | "tenant" | "workspace" | "environment" | "businessUnit"

    interface ContextComparison {
      readonly name: string
      readonly op: "eq" | "ne" | "lt" | "le" | "gt" | "ge"
      readonly value: boolean | number | string
    }

    type ResourceCompareOp = "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "in"
    type ResourceOperand = boolean | number | string | { readonly principalAttribute: string }

    interface ResourceComparison {
      readonly name: string
      readonly op: ResourceCompareOp
      readonly value: ResourceOperand
    }

    /** One typed AccessIR conjunct. */
    type AccessCondition =
      | { readonly marking: { readonly anyOf: ReadonlyArray<string> } }
      | { readonly role: { readonly anyOf: ReadonlyArray<string> } }
      | { readonly rel: { readonly from: "resource" | "principal"; readonly role: string } }
      | { readonly context: ContextComparison }
      | { readonly resource: ResourceComparison }

    interface AccessRuleSpec {
      readonly ruleId: string
      /** Actions by reference, or (for platform actions such as `norn.read`) by id. */
      readonly actions: ReadonlyArray<ActionDefinition<unknown> | string>
      readonly resourceType: TypeDefinition<unknown> | string
      readonly scope: { readonly dimensions: ReadonlyArray<ScopeDimension> }
      readonly allOf?: ReadonlyArray<AccessCondition>
    }

    /** Opaque access policy definition; the syntax compiler lowers the call. */
    interface AccessDefinition {
      readonly _tag: "NornAccess"
    }

    /** An AccessIR policy (`policyId`, permit rules; compiled to Cedar by the checker). */
    function access(meta: {
      readonly id: string
      readonly title: string
      readonly version: string
      readonly rules: ReadonlyArray<AccessRuleSpec>
    }): AccessDefinition

    /** One type's labels in a standalone catalog. */
    interface TypeLabels extends Localized {
      /** Property id → label. */
      readonly properties?: { readonly [property: string]: LocaleText }
      /** Enum property id → stored value → label. */
      readonly values?: { readonly [property: string]: { readonly [value: string]: LocaleText } }
    }
    interface LabelsDefinition { readonly _tag: "NornLabels" }
    /**
     * A standalone labels catalog: a tenant or locale pack labels definitions another
     * layer owns, without touching them (LABEL-LOCALE-1). Its labels replace the
     * installed ones for the same target and locale; the Rust checker resolves targets.
     */
    function labels(meta: {
      readonly id: string
      readonly title: string
      readonly version: string
      /** Locales this layer serves; definitions with no label in one are listed as a warning. */
      readonly requiredLocales?: ReadonlyArray<string>
      /** Locales whose labels were proposed and still need review. */
      readonly needsReview?: ReadonlyArray<string>
      /** Object type or interface name → labels. */
      readonly types?: { readonly [objectType: string]: TypeLabels }
      /** Action id → labels. */
      readonly actions?: { readonly [actionId: string]: Localized }
    }): LabelsDefinition

    /** Data-only law over ontology-bound ZEN predicates. The Rust schema is authoritative. */
    interface LawDefinition { readonly _tag: "NornLaw" }
    /** What a law lambda reads as `self`: the subject's properties and, one level deep, its links. */
    type LinkTargetProperties<K> = K extends { readonly target: TypeDefinition<infer T, any> } ? T : {}
    type LinkValues<L extends ReadonlyArray<LinkSpec>> = {
      readonly [K in L[number] as K["id"]]: K["cardinality"]["to"] extends "many"
        ? ReadonlyArray<LinkTargetProperties<K>>
        : K["cardinality"] extends { readonly required: true } ? LinkTargetProperties<K> : LinkTargetProperties<K> | null
    }
    type SelfOf<D> = D extends TypeDefinition<infer T, infer L> ? T & LinkValues<L> : D extends InterfaceDefinition<infer T> ? T : never
    type LawPredicate<D> = (self: SelfOf<NoInfer<D>>) => boolean
    interface LawSpecBase<D> {
      /** The subject: an object type or an interface (every implementation inherits the law). */
      readonly on: D
      /** Condition under which the law applies. */
      readonly when?: LawPredicate<D>
      /** `before` is a bare read of a timestamp property only (`self => self.due_at`). */
      readonly within?: { readonly days?: number; readonly hours?: number } | { readonly before: (self: SelfOf<NoInfer<D>>) => string }
      /** Default "error". */
      readonly severity?: "error" | "warning"
      /** Rationale, required. */
      readonly because: string
      /** Codemod only: keep an existing law id instead of `<namespace>.<Type>.<name>`. */
      readonly id?: string
    }
    /** Exactly one of `must` / `never` / `eventually`. */
    type LawSpec<D> = LawSpecBase<D> & (
      | { readonly must: LawPredicate<D>; readonly never?: undefined; readonly eventually?: undefined }
      | { readonly never: LawPredicate<D>; readonly must?: undefined; readonly eventually?: undefined }
      | { readonly eventually: LawPredicate<D>; readonly must?: undefined; readonly never?: undefined }
    )
    /**
     * A law over a subject type (dx-spec §4.5). Id, scope, `objectTypes`,
     * `lawVersion`, owner, provenance and the entity dependency are inferred;
     * the lambdas compile statically and never run.
     */
    function law<const D extends TypeDefinition<any, any> | InterfaceDefinition<any>>(name: string, spec: LawSpec<D>): LawDefinition
    function law(meta: Localized & {
      readonly id: string
      readonly title: string
      readonly version: string
      /** `condition` and a predicate `response.expression` may be typed lambdas (`Norn.expression(Subject, self => ...)`). */
      readonly law: Omit<import("../src/builder/law.ts").LawIR, "condition" | "response"> & {
        readonly condition?: string | Expression<boolean>
        readonly response: Exclude<import("../src/builder/law.ts").LawResponse, { readonly kind: "predicate" }> | { readonly kind: "predicate"; readonly expression: string | Expression<boolean> }
      }
      readonly dependencies: import("../src/builder/law.ts").LawDef["dependencies"]
    }): LawDefinition

    /** Opaque decision definition; the syntax compiler lowers the call. */
    interface DecisionDefinition<I = unknown, O = unknown> {
      readonly __types?: { readonly input: I; readonly output: O }
      readonly _tag: "NornDecision"
      /** Metadata stays type-only; the syntax compiler reads these literals without executing the value. */
      readonly id: string
      readonly artifactId?: string
      readonly version: string
    }

    /** A committed ledger event that triggers a `decide` rule. */
    type RuleTriggerSpec =
      | { readonly on: "transitioned"; readonly type: TypeDefinition<unknown> | string; readonly transition: string }
      | { readonly on: "created" | "deleted"; readonly type: TypeDefinition<unknown> | string }
      | { readonly on: "observed"; readonly source: string }

    /**
     * One slice entry: the trigger object, one object keyed by a property of an
     * earlier entry, or every target of a link of an earlier entry as a bounded
     * list (#3079). `optional` omits a property-keyed entry whose key is absent.
     */
    interface SliceSpec {
      readonly name: string
      readonly type: TypeDefinition<unknown> | string
      readonly key:
        | "triggerObject"
        | { readonly property: { readonly from: string; readonly property: string } }
        | { readonly link: { readonly from: string; readonly link: string } }
        | { readonly parameter: { readonly from: string; readonly settingKey: string; readonly effectiveAt: string; readonly precedence: ReadonlyArray<Readonly<Partial<Record<`${ScopeDimension}Id`, string>>>>; readonly defaultFrom?: string } }
      readonly optional?: boolean
    }

    /** JSON data, as a JDM graph carries it. */
    type JsonData = null | boolean | number | string | ReadonlyArray<JsonData> | { readonly [key: string]: JsonData }

    /** A JDM decision graph, exactly as `jdm-editor` exports it; cells are ZEN Expression text. */
    interface JdmGraph {
      readonly contentType?: string
      readonly nodes: ReadonlyArray<{ readonly id: string; readonly name: string; readonly type: string; readonly [member: string]: JsonData }>
      readonly edges: ReadonlyArray<{ readonly id: string; readonly sourceId: string; readonly targetId: string; readonly [member: string]: JsonData }>
      readonly imports?: ReadonlyArray<string>
      readonly settings?: JsonData
    }

    interface DecisionMeta<I extends Schema.Top, O extends Schema.Top> extends Localized {
      /** `ruleId` (`commerce.rule.sales_order_credit_hold`). */
      readonly id: string
      /** The artifact id; defaults to `<id>.rule`. */
      readonly artifactId?: string
      readonly title: string
      readonly version: string
      readonly contract: "decide" | "sub_decision"
      /** Required sub-decision references, either local definitions or explicit artifact/version pins. */
      readonly dependencies?: ReadonlyArray<DecisionDefinition | import("../src/builder/envelope.ts").DependencyPin>
      readonly triggers?: ReadonlyArray<RuleTriggerSpec>
      readonly slice?: ReadonlyArray<SliceSpec>
      /** The decision's input and output contracts: Effect Schema structs, lowered to closed schemas. */
      readonly input: I
      readonly output: O
      readonly scopeRequired: ReadonlyArray<string>
      readonly graph?: UnsafeJdmGraph
    }

    /** A RuleIR v2 decision (T7-TS-3). */
    function decision<I extends Schema.Top, O extends Schema.Top, const Result extends O["Type"] = O["Type"]>(
      meta: DecisionMeta<I, O>,
      body?: (input: I["Type"]) => Result,
    ): DecisionDefinition<I["Type"], O["Type"]>

    interface Expression<T> { readonly _tag: "NornExpression"; readonly __type?: T }
    /** Syntax-only expression; the callback is never executed. */
    function expression<S extends Schema.Top, T>(schema: S, body: (input: S["Type"]) => T, options?: { readonly root: "context" }): Expression<T>
    /** Explicit untyped escape hatch. Reported as a warning; Rust still checks admission. */
    function unsafeZen<T = string>(text: string): T & Expression<T>
    function unsafeZen(graph: JdmGraph): UnsafeJdmGraph
    type UnsafeJdmGraph = JdmGraph & { readonly __unsafeZen: true }

    /** Call a pinned sub-decision in a top-level const inside a decision callback. */
    function evaluate<I, O>(decision: DecisionDefinition<I, O>, input: I): O

    /** A bounded, ordered application of a pinned step; compiled, never executed in JS. */
    function orderedFold<I extends object, S extends object>(
      step: DecisionDefinition<{ readonly accumulator: S; readonly item: I; readonly index: number }, S>,
      input: { readonly items: ReadonlyArray<I>; readonly initial: S },
      options: {
        readonly identity: keyof I & string
        readonly order: ReadonlyArray<{ readonly property: keyof I & string; readonly kind: "string" | "integer" }>
        readonly maxItems: number
        readonly maxAccumulatorBytes: number
        readonly arithmetic: { readonly scale: number; readonly rounding: "refuse" | "toward_zero" | "half_even" | "half_away_from_zero" }
      },
    ): S

    /** Pure calendar-date comparison; lowers to explicit ZEN date constructors. */
    function dateBefore(left: string, right: string): boolean
    /** Explicit checked calendar conversion; malformed input fails evaluation. */
    function parseDate(text: string): string
    /** Explicit checked RFC 3339 conversion; requires a known offset, returns canonical UTC. */
    function parseTimestamp(text: string): string
    /** UTF-8 byte count, matching the canonical ZEN profile. */
    function byteLength(text: string): number
    /** Sum of a list of numbers (`sum`); an empty list sums to 0. */
    function sum(values: ReadonlyArray<number>): number
    /** Signed calendar days from `start` to `end` (`days_between`); raw date text is parsed. */
    function daysBetween(start: string, end: string): number
    /** The calendar date `days` after `date` (`add_days`); a parsed date, comparable with other parsed dates. */
    function addDays(date: string, days: number): string

    interface SourceSchemaSpec {
      readonly id: string
      readonly title: string
      readonly version: string
      readonly source: { readonly system: string; readonly instance: string; readonly version?: string }
      readonly observedAt: string
      readonly adapter: { readonly name: string; readonly version: string }
      readonly rawMetadata: { readonly uri: string; readonly sha256: string }
      readonly datasets: ReadonlyArray<{ readonly id: string; readonly nativeName: string; readonly namespace?: string; readonly kind: "table" | "view" | "sheet" | "file" | "stream" | "document" | "child_document" }>
      readonly fields: ReadonlyArray<{
        readonly dataset: string; readonly name: string; readonly ordinal: number; readonly nativeType: string
        readonly typeCandidate: { readonly type: "string" | "text" | "integer" | "decimal" | "boolean" | "date" | "datetime" | "time" | "duration" | "json" | "binary" | "reference" | "unknown"; readonly origin: "declared" | "inferred" }
        readonly required?: boolean; readonly markings?: ReadonlyArray<string>
      }>
      /** Only declared sets narrow mapping inputs; candidates remain evidence. */
      readonly codeSets?: ReadonlyArray<{
        readonly id: string; readonly dataset: string; readonly field: string
        readonly origin: "declared" | "observed_candidate"
        readonly values: ReadonlyArray<{ readonly value: string; readonly label?: string }>
      }>
      /** Declared constraints; a `parent_child` one names the parent's collection field a nested map reads. */
      readonly constraints?: ReadonlyArray<{
        readonly id: string; readonly kind: "primary_key" | "unique" | "foreign_key" | "logical_reference" | "parent_child"
        readonly dataset: string; readonly fields: ReadonlyArray<string>
        readonly target?: { readonly dataset: string; readonly fields: ReadonlyArray<string> }
        readonly collection?: string; readonly origin: "declared" | "inferred"
      }>
    }
    function sourceSchema(meta: SourceSchemaSpec): { readonly _tag: "NornSourceSchema" }

    /** One source-manifest entry (norn#56): pinned release, licence, redistribution, import mode, importer and checksums. */
    interface OntologySourceSpec {
      readonly sourceId: string
      readonly releaseId: string
      readonly releasedAt: string
      readonly retrievedAt: string
      readonly canonicalUrl: string
      readonly license: string
      readonly redistribution: "reference_only" | "vendored"
      readonly importMode: "import" | "map_only" | "study_only"
      readonly importer?: { readonly name: string; readonly version: string }
      readonly files?: ReadonlyArray<{ readonly path: string; readonly sha256: string }>
    }
    /** A closed-world, semantic-only ontology pack; lowers to a `business_pack` artifact with `packKind: "ontology"`. */
    interface OntologyPackSpec {
      readonly id: string
      readonly title: string
      readonly version: string
      readonly sources: ReadonlyArray<OntologySourceSpec>
      readonly terms: ReadonlyArray<{
        readonly termId: string; readonly label: string; readonly definition: string
        readonly termKind: "class" | "property" | "value_object" | "concept_scheme_root" | "action" | "predicate"
        readonly status: "active" | "deprecated" | "superseded"; readonly semanticAuthority: "semantic_only"
        readonly parentTermId?: string; readonly supersededBy?: string
        /** The class a property or action belongs to. */
        readonly domainTermId?: string
      }>
      readonly mappings: ReadonlyArray<{
        readonly mappingId: string; readonly termId: string; readonly sourceId: string; readonly externalIri: string
        readonly relation: "exact_match" | "close_match" | "broad_match" | "narrow_match" | "related_match"
        readonly justification: string; readonly evidenceRefs: ReadonlyArray<string>; readonly confidenceBasisPoints: number
        readonly reviewedBy: string; readonly reviewedAt: string
        readonly supersededByMappingId?: string; readonly ratifiedBy?: string; readonly ratifiedAt?: string
      }>
      readonly shapes: ReadonlyArray<{
        readonly shapeId: string; readonly termId: string; readonly closed: true
        readonly fields: ReadonlyArray<{
          readonly fieldId: string
          readonly valueType: "string" | "boolean" | "integer" | "decimal" | "date_time" | "iri" | "term_ref" | "external_identifier" | "category_ref"
          readonly targetTermId?: string; readonly minCount: number; readonly maxCount: number
        }>
      }>
    }
    function ontologyPack(meta: OntologyPackSpec): { readonly _tag: "NornOntologyPack" }

    /** Checked correspondences from one ontology pack's terms into another's; lowers to a `business_pack` with `packKind: "link"`. */
    interface PackLinkSpec {
      readonly id: string
      readonly title: string
      readonly version: string
      readonly fromPack: { readonly packId: string; readonly version: string }
      readonly toPack: { readonly packId: string; readonly version: string }
      readonly correspondences: ReadonlyArray<{
        readonly correspondenceId: string; readonly fromTermId: string
        readonly relation: "equivalent" | "subsumed_by" | "subsumes" | "derived"
        readonly toTermIds: ReadonlyArray<string>; readonly justification: string
        readonly evidenceRefs: ReadonlyArray<string>; readonly confidenceBasisPoints: number
        readonly reviewedBy: string; readonly reviewedAt: string
        readonly transformRef?: string; readonly rulingRef?: string
      }>
      readonly residuals?: ReadonlyArray<{ readonly termId: string; readonly reason: "system_specific" | "canon_gap" | "non_conforming"; readonly note: string }>
    }
    function packLink(meta: PackLinkSpec): { readonly _tag: "NornPackLink" }

    /** Opaque mapping definition; the syntax compiler lowers the call. */
    interface MappingDefinition {
      readonly _tag: "NornMapping"
    }

    /** One object map: a logical source dataset onto a target type through an interface. Transforms are ZEN Expression text over `input`. */
    interface ObjectMapSpec {
      readonly id: string
      readonly source: { readonly dataset: string; readonly nestedIn?: { readonly dataset: string; readonly field: string } }
      readonly target: { readonly type: TypeDefinition<unknown>; readonly interface: InterfaceDefinition<unknown> }
      readonly when?: Expression<boolean>
      readonly subject: string | Expression<string>
      readonly sourceRecord: string | Expression<string>
      readonly properties?: ReadonlyArray<{ readonly property: string; readonly value: string | Expression<unknown> }>
      /** Link patches: the link, the map that produces its target, and the target key expression. */
      readonly links?: ReadonlyArray<{ readonly link: string; readonly targetMap: string; readonly key: string }>
      /** Lifecycle maps: a state or transition expression, and the state an observed delete lands in. */
      readonly lifecycles?: ReadonlyArray<{ readonly property: string; readonly state?: string; readonly transition?: string; readonly onDelete?: string }>
      readonly gaps?: ReadonlyArray<{ readonly member: string; readonly reason: "not_in_source" | "defaulted" | "deferred" | "derived_elsewhere"; readonly note?: string }>
      readonly ignoredFields?: ReadonlyArray<{ readonly field: string; readonly reason: string }>
    }

    /** A MappingIR (`norn.mapping/1`) from a pinned source schema onto declared types. */
    function mapping(meta: {
      /** The artifact id (`erpnext.sales_order`). */
      readonly id: string
      readonly title: string
      readonly version: string
      /** The pinned source schema: its artifact id, version, and envelope hash (verified by the checker). */
      readonly source: { readonly schemaId: string; readonly version: string; readonly sha256: string }
      /** Exact vendor releases/revisions and separate API-access terms evidence. */
      readonly sourceContract?: { readonly kind: "generic" } | {
        readonly kind: "vendor"
        readonly system: string
        readonly versions: readonly [string, ...string[]]
        readonly apiTerms: { readonly uri: string; readonly sha256: string }
      }
      readonly interfaces: ReadonlyArray<InterfaceDefinition<unknown>>
      readonly maps: ReadonlyArray<ObjectMapSpec>
    }): MappingDefinition
    /** Authentication declarations contain logical secret references, never resolved credentials. */
    type ConnectorAuth =
      | { readonly kind: "none" }
      | { readonly kind: "api_key"; readonly authId: string; readonly placement: "header" | "query"; readonly name: string; readonly secretRef: string; readonly prefix?: string; readonly format?: { readonly kind: "raw" } | { readonly kind: "pair"; readonly separator: string } }
      | { readonly kind: "oauth2"; readonly authId: string; readonly authorizationUrl: string; readonly tokenUrl: string; readonly scopes: ReadonlyArray<string>; readonly clientSecretRef?: string }

    interface ResourceOptions {
      readonly resourceId: string
      readonly endpoint: string
      readonly itemsPath: string
      readonly missingItemsIsEmpty?: boolean
      readonly queryParams?: Readonly<Record<string, string>>
    }

    /** Read-only HTTP resources in the existing ConnectorIR. */
    type ConnectorResource =
      | (ResourceOptions & { readonly kind: "paginated"; readonly method: "GET" | "POST"; readonly pageParam: string; readonly pageSizeParam?: string; readonly pageMode?: "page" | "offset"; readonly pageStart?: number; readonly pageSize?: number })
      | (ResourceOptions & { readonly kind: "cursor"; readonly cursorParam: string; readonly nextCursorPath: string })
      | (ResourceOptions & { readonly kind: "delta"; readonly sinceParam: string; readonly updatedAtPath: string })
      | { readonly kind: "webhook"; readonly resourceId: string; readonly eventType: string; readonly payloadPath: string }
      | { readonly kind: "csv_import"; readonly resourceId: string; readonly delimiter: "," | ";" | "\t"; readonly columns: ReadonlyArray<string> }

    type ConnectorMapping =
      | { readonly kind: "field"; readonly resourceId: string; readonly sourcePath: string; readonly targetObjectKind: string; readonly targetField: string }
      | { readonly kind: "external_id"; readonly resourceId: string; readonly sourcePath: string; readonly targetObjectKind: string; readonly externalSource: string }

    interface ConnectorSpec {
      readonly id: string
      readonly title: string
      readonly version: string
      readonly auth: ConnectorAuth
      readonly resources: ReadonlyArray<ConnectorResource>
      readonly mappings: ReadonlyArray<ConnectorMapping>
      readonly cursor?: { readonly cursorId: string; readonly resourceId: string; readonly storageKey: string }
      readonly deadLetterPolicy?: { readonly maxFailures: number; readonly route: "ledger" | "queue" }
      /** Ordered feeds lower to the existing provider-neutral norn.poll/1 extension. */
      readonly poll?: { readonly feeds: ReadonlyArray<{ readonly resourceId: string; readonly recordKeyPath?: string; readonly fanout?: { readonly parentResourceId: string; readonly pathBinding: string; readonly keyPath: string } }> }
    }
    interface ConnectorDefinition { readonly _tag: "NornConnector" }
    /** Declarative read-only connector; host configuration supplies endpoints and secret resolution. */
    function connector(meta: ConnectorSpec): ConnectorDefinition
  }
}
