import { readFile } from "node:fs/promises";

const html = await readFile(new URL("../dist/index.html", import.meta.url), "utf8");
if (!html.includes("src=") || !html.includes("Orchard")) throw new Error("Built Orchard UI is missing its entrypoint.");
console.log("Orchard UI smoke passed: built entrypoint present.");
