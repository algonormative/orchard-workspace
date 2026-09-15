import { readFile, writeFile } from "node:fs/promises";

const lock = JSON.parse(await readFile(new URL("../package-lock.json", import.meta.url), "utf8"));
const rows = [["package", "version", "declared_license", "delivery_role", "note"]];
for (const [path, metadata] of Object.entries(lock.packages)) {
  if (!path || !path.startsWith("node_modules/")) continue;
  const name = path.slice("node_modules/".length);
  const role = name === "vite" ? "shipped" : "development_only";
  const note = name === "vite"
    ? "Vite modulepreload helper is present in ui/dist; its full upstream notice ships in FRONTEND_THIRD_PARTY_LICENSES.txt"
    : "Build or browser-test dependency; no package source is included in ui/dist";
  rows.push([name, metadata.version || "", metadata.license || "UNKNOWN", role, note]);
}
const [header, ...packages] = rows;
packages.sort((left, right) => left[0].localeCompare(right[0]));
await writeFile(new URL("../../third-party/frontend-license-inventory.tsv", import.meta.url), `${[header, ...packages].map((row) => row.map((value) => String(value).replaceAll("\t", " ")).join("\t")).join("\n")}\n`);
