import { registerBones } from "boneyard-js/react"
import decisionsListBones from "./decisions-list.bones.json"
import homeInboxBones from "./home-inbox.bones.json"
import homeKeyNumbersBones from "./home-key-numbers.bones.json"
import recordsTableBones from "./records-table.bones.json"
import semanticMapBones from "./semantic-map.bones.json"
import sourcesOverviewBones from "./sources-overview.bones.json"

/**
 * Side-effect registration of all first-party Studio UI-shape skeletons (`boneyard-js`).
 * Imported once from `src/main.tsx` so `<BoneSkeleton name="..." loading />` renders
 * exact responsive placeholder geometry immediately with zero runtime DOM measurement.
 */
registerBones({
  "home-inbox": homeInboxBones,
  "home-key-numbers": homeKeyNumbersBones,
  "sources-overview": sourcesOverviewBones,
  "records-table": recordsTableBones,
  "semantic-map": semanticMapBones,
  "decisions-list": decisionsListBones,
})
