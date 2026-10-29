// Hostile lookalike: exports a local object named `Effect` whose members merely
// forward to the real ones. Spelling matches; resolved symbols do not.
import { Effect as Real } from "effect"

export const Effect = {
  gen: Real.gen,
  succeed: Real.succeed,
}
