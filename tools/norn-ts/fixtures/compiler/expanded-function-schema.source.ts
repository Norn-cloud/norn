import { Effect } from "effect"
import { Norn } from "@norn/authoring"
interface Level0 { readonly value: string }
interface Level1 { readonly left: Level0; readonly right: Level0 }
interface Level2 { readonly left: Level1; readonly right: Level1 }
interface Level3 { readonly left: Level2; readonly right: Level2 }
interface Level4 { readonly left: Level3; readonly right: Level3 }
interface Level5 { readonly left: Level4; readonly right: Level4 }
interface Level6 { readonly left: Level5; readonly right: Level5 }
interface Level7 { readonly left: Level6; readonly right: Level6 }
interface Level8 { readonly left: Level7; readonly right: Level7 }
interface Level9 { readonly left: Level8; readonly right: Level8 }
interface Level10 { readonly left: Level9; readonly right: Level9 }
interface Level11 { readonly left: Level10; readonly right: Level10 }
interface Level12 { readonly left: Level11; readonly right: Level11 }
interface Level13 { readonly left: Level12; readonly right: Level12 }
interface Level14 { readonly left: Level13; readonly right: Level13 }
interface Level15 { readonly left: Level14; readonly right: Level14 }
interface Level16 { readonly left: Level15; readonly right: Level15 }
interface Level17 { readonly left: Level16; readonly right: Level16 }
interface Level18 { readonly left: Level17; readonly right: Level17 }
interface Level19 { readonly left: Level18; readonly right: Level18 }
interface Level20 { readonly left: Level19; readonly right: Level19 }
interface Level21 { readonly left: Level20; readonly right: Level20 }
interface Level22 { readonly left: Level21; readonly right: Level21 }
interface Level23 { readonly left: Level22; readonly right: Level22 }
interface Level24 { readonly left: Level23; readonly right: Level23 }
interface Level25 { readonly left: Level24; readonly right: Level24 }
const producer = Norn.fn<{}, Level25>({ id: "billing.producer", version: "1.0.0" })
export default Norn.flow({ id: "billing.expanded_schema", version: "1.0.0" }, Effect.gen(function* () {
  yield* Norn.perform("produce", producer, {})
  return true
}))
