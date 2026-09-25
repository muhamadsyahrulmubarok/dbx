export function nextLockEnabled(input: { available: boolean; outcome: "verified" | "canceled" | "unavailable" }): "enable" | "keep" | "unavailable" {
  if (!input.available || input.outcome === "unavailable") return "unavailable";
  if (input.outcome === "verified") return "enable";
  return "keep";
}
