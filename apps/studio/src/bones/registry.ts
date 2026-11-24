import { registerBones } from "boneyard-js/react"
import type { ResponsiveBones } from "boneyard-js"
import type { StudioBoneName } from "@/components/bone-skeleton"
import decisionsListBones from "./decisions-list.bones.json"
import homeInboxBones from "./home-inbox.bones.json"
import homeKeyNumbersBones from "./home-key-numbers.bones.json"
import recordsTableBones from "./records-table.bones.json"
import semanticMapBones from "./semantic-map.bones.json"
import sourcesOverviewBones from "./sources-overview.bones.json"

/**
 * First-party Studio UI-shape skeletons (`boneyard-js`), keyed by viewport width (`390`, `768`, `1280`)
 * with `0..100`% `x`/`w` coordinates. Exported directly for `initialBones` AND registered via
 * `registerBones(...)` so Vite chunking can never split or tree-shake the bone definitions.
 */
export const STUDIO_BONES: Record<StudioBoneName, ResponsiveBones> = {
  "home-inbox": homeInboxBones,
  "home-key-numbers": homeKeyNumbersBones,
  "sources-overview": sourcesOverviewBones,
  "records-table": recordsTableBones,
  "semantic-map": semanticMapBones,
  "decisions-list": decisionsListBones,
}

registerBones(STUDIO_BONES)
