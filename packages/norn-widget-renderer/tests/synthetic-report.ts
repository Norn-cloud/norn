/** Entirely fictional evidence, generated without business records. */
export function syntheticReport() {
  return {
    format: "norn.analysis.report/1", numericEncoding: "binary64-text/1", state: "descriptive", interpretation: "associational",
    obligations: [{ id: "independence", label: "attested", analyst: "synthetic-analyst", statement: "Fictional observations." }],
    result: { n: "6", slope: "9007199254740993.125", diagnostics: [{ method: "sample_size_min_20", passed: false }],
      plots: { residuals: [["-1e308", "-2"], ["1e308", "3"]], qq: [["-1", "-2"], ["1", "3"]], cooksDistance: ["0.01", "0.02"] } },
  }
}
