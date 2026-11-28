//! Embedded, pinned offline `effect` `TypeScript` declarations for standalone authoring.
//!
//! When a downstream Norn repository has no on-disk `tools/norn-ts/node_modules/effect`,
//! [`BoundedVfs::mount_embedded_effect_stub`] mounts these minimal declarations under
//! [`PINNED_EFFECT_VIRTUAL_ROOT`] so every symbol admitted by the `norn-effect` profile
//! resolves to its exact pinned canonical identity (`effect/<subpath>#<name>`) and
//! typechecks cleanly offline.

use std::collections::BTreeMap;

use serde_json::json;

use crate::vfs::{PINNED_EFFECT_VIRTUAL_PREFIX, path_under_prefix};
use crate::vfs_mount::{StagedMount, stage_file_content};
use crate::{AuthoringError, BoundedVfs, PINNED_EFFECT_VIRTUAL_ROOT, Profile};

const PIPEABLE_DTS: &str = r"export interface Pipeable {
  pipe<A>(this: A): A
  pipe<A, B>(this: A, ab: (a: A) => B): B
  pipe<A, B, C>(this: A, ab: (a: A) => B, bc: (b: B) => C): C
  pipe<A, B, C, D>(this: A, ab: (a: A) => B, bc: (b: B) => C, cd: (c: C) => D): D
  pipe<A, B, C, D, E>(
    this: A,
    ab: (a: A) => B,
    bc: (b: B) => C,
    cd: (c: C) => D,
    de: (d: D) => E,
  ): E
  pipe<A, B, C, D, E, F>(
    this: A,
    ab: (a: A) => B,
    bc: (b: B) => C,
    cd: (c: C) => D,
    de: (d: D) => E,
    ef: (e: E) => F,
  ): F
  pipe<A>(this: A, ...fns: ReadonlyArray<(arg: any) => any>): any
}
";

const FUNCTION_DTS: &str = r"export declare function pipe<A>(a: A): A
export declare function pipe<A, B>(a: A, ab: (a: A) => B): B
export declare function pipe<A, B, C>(a: A, ab: (a: A) => B, bc: (b: B) => C): C
export declare function pipe<A, B, C, D>(
  a: A,
  ab: (a: A) => B,
  bc: (b: B) => C,
  cd: (c: C) => D,
): D
export declare function pipe<A, B, C, D, E>(
  a: A,
  ab: (a: A) => B,
  bc: (b: B) => C,
  cd: (c: C) => D,
  de: (d: D) => E,
): E
export declare function pipe<A, B, C, D, E, F>(
  a: A,
  ab: (a: A) => B,
  bc: (b: B) => C,
  cd: (c: C) => D,
  de: (d: D) => E,
  ef: (e: E) => F,
): F
export declare function pipe<A>(a: A, ...fns: ReadonlyArray<(arg: any) => any>): any
export declare function identity<A>(a: A): A
export declare function constVoid(): void
";

const DURATION_DTS: &str = r#"export type Unit =
  | "nanos"
  | "micros"
  | "millis"
  | "seconds"
  | "minutes"
  | "hours"
  | "days"
  | "weeks"

export interface Duration {
  readonly _tag: "Duration"
}

export type DurationInput = number | bigint | `${number} ${string}` | Duration

export declare function millis(n: number): Duration
export declare function seconds(n: number): Duration
export declare function minutes(n: number): Duration
export declare function hours(n: number): Duration
export declare function days(n: number): Duration
"#;

const OPTION_DTS: &str = r#"import type { Pipeable } from "./Pipeable"

export interface None<A> extends Pipeable {
  readonly _tag: "None"
  readonly _A?: () => A
}

export interface Some<A> extends Pipeable {
  readonly _tag: "Some"
  readonly value: A
}

export type Option<A> = None<A> | Some<A>

export declare function some<A>(value: A): Option<A>
export declare function none<A = never>(): Option<A>
export declare function isSome<A>(self: Option<A>): self is Some<A>
export declare function isNone<A>(self: Option<A>): self is None<A>
"#;

const EITHER_DTS: &str = r#"import type { Pipeable } from "./Pipeable"

export interface Left<L, R> extends Pipeable {
  readonly _tag: "Left"
  readonly left: L
  readonly _R?: () => R
}

export interface Right<L, R> extends Pipeable {
  readonly _tag: "Right"
  readonly right: R
  readonly _L?: () => L
}

export type Either<R, L = never> = Left<L, R> | Right<L, R>

export declare function right<R, L = never>(right: R): Either<R, L>
export declare function left<L, R = never>(left: L): Either<R, L>
export declare function isRight<R, L>(self: Either<R, L>): self is Right<L, R>
export declare function isLeft<R, L>(self: Either<R, L>): self is Left<L, R>
"#;

const DATA_DTS: &str = r"export interface YieldableError extends Error {
  readonly _tag: string
}

export declare function TaggedError<Tag extends string>(
  tag: Tag,
): {
  new <A extends Record<string, any> = {}>(
    ...args: {} extends A ? [args?: A] : [args: A]
  ): YieldableError & { readonly _tag: Tag } & Readonly<A>
}
";

const SCHEDULE_DTS: &str = r#"import type { DurationInput } from "./Duration"
import type { Pipeable } from "./Pipeable"

export interface Schedule<Out = any, In = unknown, R = never> extends Pipeable {
  readonly _tag: "Schedule"
  readonly _Out?: () => Out
  readonly _In?: (_: In) => void
  readonly _R?: () => R
}

export declare function recurs(n: number): Schedule<number>
export declare function spaced(duration: DurationInput): Schedule<number>
export declare function exponential(base: DurationInput, factor?: number): Schedule<number>
export declare function max<Schedules extends ReadonlyArray<Schedule<any, any, any>>>(
  schedules: Schedules,
): Schedule<any>

export declare const forever: Schedule<number>
export declare function jittered<O, I, R>(self: Schedule<O, I, R>): Schedule<O, I, R>
declare function while_<O, I, R>(
  f: (input: I) => boolean,
): (self: Schedule<O, I, R>) => Schedule<O, I, R>
export { while_ as while }
"#;

const EFFECT_DTS: &str = r#"import type { DurationInput } from "./Duration"
import type { Pipeable } from "./Pipeable"
import type { Schedule } from "./Schedule"

export interface YieldWrap<T> {
  readonly _tag: "YieldWrap"
  readonly value: T
}

export interface Effect<A, E = never, R = never> extends Pipeable {
  readonly _A?: () => A
  readonly _E?: () => E
  readonly _R?: () => R
  [Symbol.iterator](): Generator<YieldWrap<Effect<A, E, R>>, A, never>
}

export type Success<T> = [T] extends [Effect<infer A, any, any>] ? A : never
export type Error<T> = [T] extends [Effect<any, infer E, any>] ? E : never
export type Services<T> = [T] extends [Effect<any, any, infer R>] ? R : never

export declare function gen<Eff extends YieldWrap<Effect<any, any, any>>, AEff>(
  f: () => Generator<Eff, AEff, never>,
): Effect<
  AEff,
  [Eff] extends [never] ? never : Eff extends YieldWrap<Effect<any, infer E, any>> ? E : never,
  [Eff] extends [never] ? never : Eff extends YieldWrap<Effect<any, any, infer R>> ? R : never
>

export declare function succeed<A>(value: A): Effect<A>
export declare function fail<E>(error: E): Effect<never, E>
export declare function sleep(duration: DurationInput): Effect<void>

export declare function map<A, B>(
  f: (a: A) => B,
): <E, R>(self: Effect<A, E, R>) => Effect<B, E, R>
export declare function map<A, E, R, B>(self: Effect<A, E, R>, f: (a: A) => B): Effect<B, E, R>

export declare function flatMap<A, B, E2, R2>(
  f: (a: A) => Effect<B, E2, R2>,
): <E1, R1>(self: Effect<A, E1, R1>) => Effect<B, E1 | E2, R1 | R2>
export declare function flatMap<A, E1, R1, B, E2, R2>(
  self: Effect<A, E1, R1>,
  f: (a: A) => Effect<B, E2, R2>,
): Effect<B, E1 | E2, R1 | R2>

export declare function catchTag<const K extends string, E, A2, E2, R2>(
  k: K,
  f: (e: Extract<E, { readonly _tag: K }>) => Effect<A2, E2, R2>,
): <A, R>(
  self: Effect<A, E, R>,
) => Effect<A | A2, Exclude<E, { readonly _tag: K }> | E2, R | R2>
export declare function catchTag<A, E, R, const K extends string, A2, E2, R2>(
  self: Effect<A, E, R>,
  k: K,
  f: (e: Extract<E, { readonly _tag: K }>) => Effect<A2, E2, R2>,
): Effect<A | A2, Exclude<E, { readonly _tag: K }> | E2, R | R2>

export declare function all<const Arg extends ReadonlyArray<Effect<any, any, any>>>(
  arg: Arg,
  options?: {
    readonly concurrency?: number | "unbounded" | "inherit"
    readonly mode?: string
  },
): Effect<
  { -readonly [K in keyof Arg]: Success<Arg[K]> },
  Error<Arg[number]>,
  Services<Arg[number]>
>

export declare function retry<Out, E, R2>(
  policy: Schedule<Out, NoInfer<E>, R2>,
): <A, R>(self: Effect<A, E, R>) => Effect<A, E, R | R2>
export declare function retry<A, E, R, Out, R2>(
  self: Effect<A, E, R>,
  policy: Schedule<Out, NoInfer<E>, R2>,
): Effect<A, E, R | R2>

export declare function runPromise(...args: ReadonlyArray<any>): any
export declare function runPromiseExit(...args: ReadonlyArray<any>): any
export declare function runSync(...args: ReadonlyArray<any>): any
export declare function runSyncExit(...args: ReadonlyArray<any>): any
export declare function runFork(...args: ReadonlyArray<any>): any
export declare function runCallback(...args: ReadonlyArray<any>): any
export declare function sync(...args: ReadonlyArray<any>): any
export declare function suspend(...args: ReadonlyArray<any>): any
export declare function callback(...args: ReadonlyArray<any>): any
export declare function promise(...args: ReadonlyArray<any>): any
export declare function tryPromise(...args: ReadonlyArray<any>): any
export declare function die(...args: ReadonlyArray<any>): any
export declare function forkChild(...args: ReadonlyArray<any>): any
export declare function forkIn(...args: ReadonlyArray<any>): any
export declare function forkScoped(...args: ReadonlyArray<any>): any
export declare function forkDetach(...args: ReadonlyArray<any>): any
export declare function race(...args: ReadonlyArray<any>): any
export declare function raceAll(...args: ReadonlyArray<any>): any
export declare function raceFirst(...args: ReadonlyArray<any>): any
export declare function timeout(...args: ReadonlyArray<any>): any
export declare function timeoutOption(...args: ReadonlyArray<any>): any
export declare function interrupt(...args: ReadonlyArray<any>): any
export declare function onInterrupt(...args: ReadonlyArray<any>): any
export declare function uninterruptible(...args: ReadonlyArray<any>): any
export declare function provide(...args: ReadonlyArray<any>): any
export declare function provideService(...args: ReadonlyArray<any>): any
export declare function service(...args: ReadonlyArray<any>): any
export declare function context(...args: ReadonlyArray<any>): any
export declare function acquireRelease(...args: ReadonlyArray<any>): any
export declare function acquireUseRelease(...args: ReadonlyArray<any>): any
export declare function addFinalizer(...args: ReadonlyArray<any>): any
export declare function ensuring(...args: ReadonlyArray<any>): any
export declare function scoped(...args: ReadonlyArray<any>): any
export declare function onExit(...args: ReadonlyArray<any>): any
export declare function forever(...args: ReadonlyArray<any>): any
export declare function repeat(...args: ReadonlyArray<any>): any
export declare function whileLoop(...args: ReadonlyArray<any>): any
export declare function eventually(...args: ReadonlyArray<any>): any
export declare function forEach(...args: ReadonlyArray<any>): any
export declare function catchIf(...args: ReadonlyArray<any>): any
export declare function filterOrFail(...args: ReadonlyArray<any>): any
export declare function when(...args: ReadonlyArray<any>): any
export declare function clockWith(...args: ReadonlyArray<any>): any
"#;

const SCHEMA_DTS: &str = r#"import type { Pipeable } from "./Pipeable"

export interface Schema<T> extends Pipeable {
  readonly Type: T
}

export namespace Schema {
  export type Type<S extends Top> = S["Type"]
}

export type Top = Schema<any>

export interface optionalKey<S extends Top> extends Pipeable {
  readonly _tag: "optionalKey"
  readonly Type: S["Type"] | undefined
}

type FieldSpec = Top | optionalKey<any>
type OptionalKeys<Fields extends Record<string, FieldSpec>> = {
  [K in keyof Fields]: Fields[K] extends optionalKey<any> ? K : never
}[keyof Fields]
type RequiredKeys<Fields extends Record<string, FieldSpec>> = Exclude<
  keyof Fields,
  OptionalKeys<Fields>
>
type FieldType<F> = F extends optionalKey<infer S> ? S["Type"] : F extends Top ? F["Type"] : never
type Simplify<T> = { [K in keyof T]: T[K] } & {}
type StructType<Fields extends Record<string, FieldSpec>> = Simplify<
  { readonly [K in RequiredKeys<Fields>]: FieldType<Fields[K]> } & {
    readonly [K in OptionalKeys<Fields>]?: FieldType<Fields[K]>
  }
>

export interface String extends Schema<string> {}
export declare const String: String
export interface Boolean extends Schema<boolean> {}
export declare const Boolean: Boolean
export interface Null extends Schema<null> {}
export declare const Null: Null
export interface Int extends Schema<number> {}
export declare const Int: Int
export interface Finite extends Schema<number> {}
export declare const Finite: Finite

export interface Literal<L extends string | number | boolean> extends Schema<L> {}
export declare function Literal<const L extends string | number | boolean>(literal: L): Literal<L>

export interface Literals<L extends ReadonlyArray<string | number | boolean>>
  extends Schema<L[number]> {}
export declare function Literals<const L extends ReadonlyArray<string | number | boolean>>(
  literals: L,
): Literals<L>

export interface Struct<Fields extends Record<string, FieldSpec>>
  extends Schema<StructType<Fields>> {}
export declare function Struct<const Fields extends Record<string, FieldSpec>>(
  fields: Fields,
): Struct<Fields>

export declare function TaggedStruct<
  const Tag extends string,
  const Fields extends Record<string, FieldSpec>,
>(tag: Tag, fields: Fields): Schema<Simplify<{ readonly _tag: Tag } & StructType<Fields>>>

export interface ClassConstructor<Self, Fields extends Record<string, FieldSpec>>
  extends Schema<StructType<Fields>> {
  new (props: StructType<Fields>): StructType<Fields>
  readonly _Self?: () => Self
}
export declare function Class<Self = never>(
  identifier: string,
): <const Fields extends Record<string, FieldSpec>>(
  fields: Fields,
) => ClassConstructor<Self, Fields>

export interface ArraySchema<S extends Top> extends Schema<ReadonlyArray<S["Type"]>> {}
declare function ArraySchema<S extends Top>(item: S): ArraySchema<S>
export { ArraySchema as Array }

export declare function Union<const Members extends ReadonlyArray<Top>>(
  members: Members,
): Schema<Members[number]["Type"]>

export declare function TaggedUnion<
  const Cases extends Record<string, Record<string, FieldSpec>>,
>(
  cases: Cases,
): Schema<
  {
    [K in keyof Cases & string]: Simplify<{ readonly _tag: K } & StructType<Cases[K]>>
  }[keyof Cases & string]
>

export declare function optionalKey<S extends Top>(schema: S): optionalKey<S>
export declare function NullOr<S extends Top>(schema: S): Schema<S["Type"] | null>
export declare function brand<const B extends string | symbol>(
  brand: B,
): <S extends Top>(self: S) => Schema<S["Type"]>

export declare function check(...args: ReadonlyArray<any>): any
export declare function refine(...args: ReadonlyArray<any>): any
export declare function decodeTo(...args: ReadonlyArray<any>): any
export declare function suspend(...args: ReadonlyArray<any>): any
export declare function declare(...args: ReadonlyArray<any>): any
export declare const Date: Schema<globalThis.Date>
export declare const Number: Schema<number>
export declare const BigInt: Schema<bigint>
"#;

const AI_TOOL_DTS: &str = r#"import type * as Schema from "../../Schema"

export interface Tool<Name extends string = string> {
  readonly _tag: "Tool"
  readonly name: Name
}

export declare function make<const Name extends string>(
  name: Name,
  options?: {
    readonly description?: string
    readonly parameters?: Schema.Top
    readonly success?: Schema.Top
    readonly failure?: Schema.Top
  },
): Tool<Name>
"#;

const AI_TOOLKIT_DTS: &str = r#"import type { Tool } from "./Tool"

export interface Toolkit<Tools extends ReadonlyArray<Tool<any>> = ReadonlyArray<Tool<any>>> {
  readonly _tag: "Toolkit"
  readonly tools: Tools
}

export declare function make<const Tools extends ReadonlyArray<Tool<any>>>(
  ...tools: Tools
): Toolkit<Tools>
"#;

const AI_INDEX_DTS: &str = r#"export * as Tool from "./Tool"
export * as Toolkit from "./Toolkit"
"#;

const INDEX_DTS: &str = r#"export * as Data from "./Data"
export * as Duration from "./Duration"
export * as Effect from "./Effect"
export * as Either from "./Either"
export * as Function from "./Function"
export * as Option from "./Option"
export * as Pipeable from "./Pipeable"
export * as Schedule from "./Schedule"
export * as Schema from "./Schema"
export { pipe } from "./Function"
"#;

const EMBEDDED_EFFECT_DECLARATIONS: &[(&str, &str)] = &[
    ("dist/index.d.ts", INDEX_DTS),
    ("dist/Pipeable.d.ts", PIPEABLE_DTS),
    ("dist/Function.d.ts", FUNCTION_DTS),
    ("dist/Duration.d.ts", DURATION_DTS),
    ("dist/Option.d.ts", OPTION_DTS),
    ("dist/Either.d.ts", EITHER_DTS),
    ("dist/Data.d.ts", DATA_DTS),
    ("dist/Schedule.d.ts", SCHEDULE_DTS),
    ("dist/Effect.d.ts", EFFECT_DTS),
    ("dist/Schema.d.ts", SCHEMA_DTS),
    ("dist/unstable/ai/index.d.ts", AI_INDEX_DTS),
    ("dist/unstable/ai/Tool.d.ts", AI_TOOL_DTS),
    ("dist/unstable/ai/Toolkit.d.ts", AI_TOOLKIT_DTS),
];

fn embedded_effect_package_json(version: &str) -> String {
    json!({
        "name": "effect",
        "version": version,
        "types": "./dist/index.d.ts",
        "exports": {
            ".": { "types": "./dist/index.d.ts" },
            "./Data": { "types": "./dist/Data.d.ts" },
            "./Duration": { "types": "./dist/Duration.d.ts" },
            "./Effect": { "types": "./dist/Effect.d.ts" },
            "./Either": { "types": "./dist/Either.d.ts" },
            "./Function": { "types": "./dist/Function.d.ts" },
            "./Option": { "types": "./dist/Option.d.ts" },
            "./Pipeable": { "types": "./dist/Pipeable.d.ts" },
            "./Schedule": { "types": "./dist/Schedule.d.ts" },
            "./Schema": { "types": "./dist/Schema.d.ts" },
            "./unstable/ai": { "types": "./dist/unstable/ai/index.d.ts" },
            "./unstable/ai/Tool": { "types": "./dist/unstable/ai/Tool.d.ts" },
            "./unstable/ai/Toolkit": { "types": "./dist/unstable/ai/Toolkit.d.ts" },
        },
    })
    .to_string()
}

impl BoundedVfs {
    /// Mounts the minimal, self-contained, pinned virtual `effect` declarations
    /// under [`PINNED_EFFECT_VIRTUAL_ROOT`] for offline standalone compilation
    /// when no on-disk `node_modules/effect` package is available.
    ///
    /// Replaces any prior mount under `/node_modules/effect/` atomically.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::VfsQuotaExceeded`] if the VFS is locked after
    /// an earlier quota breach or exceeds pinned caps, or
    /// [`AuthoringError::InvalidPath`] if the virtual root conflicts with an
    /// existing caller file.
    pub fn mount_embedded_effect_stub(
        &mut self,
        profile: &Profile,
    ) -> Result<usize, AuthoringError> {
        if self.quota_breached() {
            return Err(AuthoringError::VfsQuotaExceeded(
                "virtual filesystem is locked after an earlier quota breach".to_owned(),
            ));
        }
        self.validate_mount_root(
            PINNED_EFFECT_VIRTUAL_ROOT,
            PINNED_EFFECT_VIRTUAL_PREFIX,
            true,
        )?;

        let mut retained_count: usize = 0;
        let mut retained_bytes: usize = 0;
        for (existing_path, existing_content) in &self.pinned_files {
            if !path_under_prefix(existing_path, PINNED_EFFECT_VIRTUAL_PREFIX) {
                retained_count = retained_count.saturating_add(1);
                retained_bytes = retained_bytes.saturating_add(existing_content.len());
            }
        }

        let mut staged = StagedMount {
            prefix_with_slash: PINNED_EFFECT_VIRTUAL_PREFIX.to_owned(),
            files: BTreeMap::new(),
            total_count: retained_count,
            next_total_bytes: retained_bytes,
            include_intrinsics: self.include_intrinsics,
        };

        let manifest_path = format!("{PINNED_EFFECT_VIRTUAL_ROOT}/package.json");
        let manifest_text = embedded_effect_package_json(&profile.pins.effect.version);
        stage_file_content(&manifest_path, manifest_text, &mut staged)?;

        for &(rel_path, content) in EMBEDDED_EFFECT_DECLARATIONS {
            let vpath = format!("{PINNED_EFFECT_VIRTUAL_ROOT}/{rel_path}");
            stage_file_content(&vpath, content.to_owned(), &mut staged)?;
        }

        let count = staged.files.len();
        self.commit_staged_mount(staged);
        Ok(count)
    }
}
