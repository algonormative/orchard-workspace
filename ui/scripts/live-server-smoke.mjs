import { mkdir, rm } from "node:fs/promises";
import path from "node:path";
import { chromium } from "playwright";

const serverUrl = process.env.ORCHARD_TEST_URL;
if (!serverUrl) throw new Error("Set ORCHARD_TEST_URL.");
const origin = new URL(serverUrl);
if (origin.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(origin.hostname)) throw new Error("ORCHARD_TEST_URL must be loopback HTTP.");
origin.pathname = "/"; origin.search = ""; origin.hash = "";

const evidenceDir = "/tmp/orchard-evidence"; await mkdir(evidenceDir, { recursive: true });
const screenshots = { workspace: path.join(evidenceDir, "workspace.png"), task: path.join(evidenceDir, "task.png"), attachment: path.join(evidenceDir, "attachment.png") };
const failureScreenshot = path.join(evidenceDir, "failure.png");
await Promise.all([...Object.values(screenshots), failureScreenshot].map((file) => rm(file, { force: true })));
const runId = `${Date.now()}-${process.pid}`;

function operation(response) {
  if (response.request().method() !== "POST" || new URL(response.url()).pathname !== "/api/call") return undefined;
  try { return JSON.parse(response.request().postData() || "{}").operation; } catch { return undefined; }
}
async function result(page, name, action) {
  const pending = page.waitForResponse((response) => operation(response) === name, { timeout: 15_000 });
  await action(); const response = await pending; const body = await response.json();
  if (!response.ok() || !("result" in body)) throw new Error(`${name} failed: HTTP ${response.status()} ${JSON.stringify(body)}`);
  return body.result;
}
async function openGroup(page, name) {
  const group = page.locator("details.tree-group").filter({ has: page.locator("summary", { hasText: name }) });
  if (!(await group.evaluate((node) => node.open))) await group.locator("summary").click();
  return group;
}

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme: "dark" });
const page = await context.newPage(); const browserErrors = [];
page.on("pageerror", (error) => browserErrors.push(`pageerror: ${error.message}`));
page.on("console", (entry) => { if (entry.type() === "error") browserErrors.push(`console: ${entry.text()}`); });

try {
  const documentResponse = await page.goto(origin.href, { waitUntil: "networkidle" });
  if (!documentResponse?.ok() || !(await page.locator('script[type="module"][src]').count())) throw new Error("Embedded UI assets did not load.");

  const selector = page.getByLabel("Workspace", { exact: true });
  await page.getByRole("heading", { name: "Start a workspace" }).or(selector).waitFor();
  if (await selector.isVisible()) await page.getByRole("button", { name: "New workspace", exact: true }).click();
  const workspaceName = `Live smoke ${runId}`; await page.getByLabel("Workspace name").fill(workspaceName);
  const created = await result(page, "workspace_create", () => page.getByRole("button", { name: "Create workspace", exact: true }).click());
  const workspace = created.workspace; const store = workspace?.task_stores?.[0];
  if (!workspace?.id || !store?.id) throw new Error(`workspace_create omitted workspace/store: ${JSON.stringify(created)}`);
  await page.getByLabel("Workspace", { exact: true }).waitFor();
  await page.getByLabel("Workspace", { exact: true }).selectOption(workspace.id);
  await page.locator("#conversations").getByRole("button", { name: "#general", exact: true }).waitFor();
  const introResponse = page.waitForResponse((response) => operation(response) === "workspace_intro", { timeout: 15_000 });
  const infoResponse = page.waitForResponse((response) => operation(response) === "workspace_info", { timeout: 15_000 });
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  if (!(await introResponse).ok() || !(await infoResponse).ok()) throw new Error("Workspace introduction did not load.");
  await page.getByRole("heading", { name: "Workspace settings", exact: true }).waitFor();
  if (!new URL(page.url()).pathname.endsWith(`/w/${workspace.id}/settings`)) throw new Error(`Settings route is not canonical: ${page.url()}`);
  await page.getByRole("button", { name: "Back to workspace", exact: true }).first().click();
  await page.screenshot({ path: screenshots.workspace, fullPage: true });

  await openGroup(page, "Tasks"); await page.getByRole("button", { name: "All tasks", exact: true }).click();
  await page.getByRole("button", { name: "New task", exact: true }).click();
  const taskTitle = `Verify live workspace ${runId}`; await page.getByLabel("Task title").fill(taskTitle);
  const made = await result(page, "task_create", () => page.getByRole("button", { name: "Create task", exact: true }).click());
  const task = made.task; if (!task?.id) throw new Error(`task_create omitted task: ${JSON.stringify(made)}`);
  await page.getByRole("button", { name: taskTitle, exact: true }).first().click();
  await page.getByLabel("Task status").selectOption("closed");
  const updated = await result(page, "task_update", () => page.getByRole("button", { name: "Update status", exact: true }).click());
  if (updated?.task?.status !== "closed") throw new Error(`task_update did not close task: ${JSON.stringify(updated)}`);
  await page.screenshot({ path: screenshots.task, fullPage: true });

  const chats = await openGroup(page, "Chats"); await chats.getByRole("button", { name: "#general", exact: true }).click();
  await page.getByLabel("Message").fill(`Live attachment ${runId}`);
  const uploaded = await result(page, "artifact_upload", () => page.locator('input[type="file"]').setInputFiles({ name: `live-${runId}.txt`, mimeType: "text/plain", buffer: Buffer.from("live attachment\n") }));
  if (uploaded?.resource?.ref?.kind !== "file" || !uploaded.resource.ref.revision) throw new Error(`artifact_upload omitted pinned file ref: ${JSON.stringify(uploaded)}`);
  await page.getByRole("button", { name: `Remove attachment live-${runId}.txt`, exact: true }).waitFor();
  const sent = await result(page, "mail_send", () => page.getByRole("button", { name: "Send", exact: true }).click());
  const attached = sent?.message?.refs?.[0]?.resource;
  if (attached?.kind !== "file" || attached.path !== `live-${runId}.txt`) throw new Error(`mail_send lost typed attachment: ${JSON.stringify(sent)}`);
  await page.getByRole("button", { name: `live-${runId}.txt`, exact: true }).click();
  await page.getByText("live attachment", { exact: true }).waitFor();
  await page.screenshot({ path: screenshots.attachment, fullPage: true });

  if (browserErrors.length) throw new Error(browserErrors.join("\n"));
  console.log(JSON.stringify({ ok: true, server: origin.origin, workspace: workspace.id, task: task.id, attachment: attached.path, screenshots }, null, 2));
} catch (error) {
  await page.screenshot({ path: failureScreenshot, fullPage: true }).catch(() => {});
  console.error(JSON.stringify({ ok: false, error: error instanceof Error ? error.message : String(error), failureScreenshot }, null, 2));
  throw error;
} finally { await context.close(); await browser.close(); }
