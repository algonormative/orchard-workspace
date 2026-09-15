import { mkdir, readFile, rm } from "node:fs/promises";
import path from "node:path";
import { chromium } from "playwright";

const serverUrl = process.env.ORCHARD_TEST_URL;
const tokenFile = process.env.ORCHARD_TEST_TOKEN_FILE;
if (!serverUrl || !tokenFile) {
  throw new Error(
    "Set ORCHARD_TEST_URL and ORCHARD_TEST_TOKEN_FILE to the loopback Orchard server and its owner credential file.",
  );
}

const origin = new URL(serverUrl);
if (origin.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(origin.hostname)) {
  throw new Error(`ORCHARD_TEST_URL must be an HTTP loopback URL, received ${serverUrl}`);
}
origin.pathname = "/";
origin.search = "";
origin.hash = "";

const ownerToken = (await readFile(tokenFile, "utf8")).trim();
if (!ownerToken) throw new Error(`Owner credential file is empty: ${tokenFile}`);

const evidenceDir = "/tmp/orchard-evidence";
await mkdir(evidenceDir, { recursive: true });
const screenshots = {
  empty: path.join(evidenceDir, "empty.png"),
  workspace: path.join(evidenceDir, "workspace.png"),
  task: path.join(evidenceDir, "task.png"),
};
const failureScreenshot = path.join(evidenceDir, "failure.png");
await Promise.all(
  [...Object.values(screenshots), failureScreenshot].map((file) => rm(file, { force: true })),
);
const runId = `${Date.now()}-${process.pid}`;
const workspaceName = `Live smoke ${runId}`;
const taskTitle = `Verify live workspace ${runId}`;
const messageBody = `Live evidence ${runId}`;
const evidenceUrl = "https://example.com/orchard-live-smoke";

function apiOperation(response) {
  if (response.request().method() !== "POST" || new URL(response.url()).pathname !== "/api/call") {
    return undefined;
  }
  try {
    return JSON.parse(response.request().postData() || "{}").operation;
  } catch {
    return undefined;
  }
}

async function actionResult(page, operation, action) {
  const responsePromise = page.waitForResponse(
    (response) => apiOperation(response) === operation,
    { timeout: 15_000 },
  );
  await action();
  const response = await responsePromise;
  const envelope = await response.json();
  if (!response.ok()) {
    throw new Error(`${operation} failed with HTTP ${response.status()}: ${JSON.stringify(envelope)}`);
  }
  if (!("result" in envelope)) {
    throw new Error(`${operation} response omitted the Rust {result} envelope: ${JSON.stringify(envelope)}`);
  }
  return envelope.result;
}

async function clickTaskAndRead(page, taskButton) {
  const show = page.waitForResponse((response) => apiOperation(response) === "task_show");
  const dependencies = page.waitForResponse(
    (response) => apiOperation(response) === "task_dependencies",
  );
  await taskButton.click();
  const [showResponse, dependencyResponse] = await Promise.all([show, dependencies]);
  const showEnvelope = await showResponse.json();
  const dependencyEnvelope = await dependencyResponse.json();
  if (!showResponse.ok() || !("result" in showEnvelope)) {
    throw new Error(`task_show returned an invalid envelope: ${JSON.stringify(showEnvelope)}`);
  }
  if (!dependencyResponse.ok() || !("result" in dependencyEnvelope)) {
    throw new Error(
      `task_dependencies returned an invalid envelope: ${JSON.stringify(dependencyEnvelope)}`,
    );
  }
  return { task: showEnvelope.result, dependencies: dependencyEnvelope.result };
}

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({
  viewport: { width: 1440, height: 1000 },
  colorScheme: "dark",
});
const page = await context.newPage();
const pageErrors = [];
page.on("pageerror", (error) => pageErrors.push(`pageerror: ${error.message}`));
page.on("console", (entry) => {
  if (entry.type() === "error") pageErrors.push(`console: ${entry.text()}`);
});

try {
  const documentResponse = await page.goto(origin.href, { waitUntil: "networkidle" });
  if (!documentResponse?.ok()) {
    throw new Error(`Embedded UI returned HTTP ${documentResponse?.status() ?? "no response"}`);
  }
  if (!(await page.locator('script[type="module"][src]').count())) {
    throw new Error("Embedded UI did not serve its module asset entrypoint.");
  }

  await page.getByLabel("Local access key").fill(ownerToken);
  const loginResponse = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" && new URL(response.url()).pathname === "/api/session",
  );
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  const login = await loginResponse;
  const loginEnvelope = await login.json();
  if (!login.ok() || loginEnvelope.authenticated !== true) {
    throw new Error(`Owner login failed: ${JSON.stringify(loginEnvelope)}`);
  }

  const workspaceSelector = page.getByLabel("Workspace", { exact: true });
  const emptyHeading = page.getByRole("heading", { name: "Start a workspace", exact: true });
  await emptyHeading.or(workspaceSelector).waitFor();
  if (await workspaceSelector.isVisible()) {
    await page.getByRole("button", { name: "New workspace", exact: true }).click();
  }
  await emptyHeading.waitFor();
  await page.screenshot({ path: screenshots.empty, fullPage: true });

  await page.getByLabel("Workspace name", { exact: true }).fill(workspaceName);
  const created = await actionResult(page, "workspace_create", () =>
    page.getByRole("button", { name: "Create workspace", exact: true }).click(),
  );
  const workspace = created?.workspace;
  if (!workspace?.id || workspace.name !== workspaceName) {
    throw new Error(`workspace_create returned an unexpected workspace: ${JSON.stringify(created)}`);
  }
  const ownedStore = workspace.task_stores?.[0];
  if (!ownedStore?.id || !ownedStore?.path) {
    throw new Error(`workspace_create did not expose its owned task store: ${JSON.stringify(created)}`);
  }
  await page.getByLabel("Workspace", { exact: true }).selectOption(workspace.id);
  await page.getByRole("button", { name: "general", exact: true }).waitFor();
  await page.screenshot({ path: screenshots.workspace, fullPage: true });

  await page.getByRole("button", { name: "Tasks", exact: true }).click();
  const storeButton = page.getByRole("button", { name: ownedStore.path, exact: true });
  await storeButton.waitFor();
  await actionResult(page, "tasks_list", () => storeButton.click());
  await page.getByRole("button", { name: "New task", exact: true }).click();
  await page.getByLabel("Task title", { exact: true }).fill(taskTitle);
  const createdTask = await actionResult(page, "task_create", () =>
    page.getByRole("button", { name: "Create task", exact: true }).click(),
  );
  const task = createdTask?.task;
  if (!task?.id || task.task_ref?.store_id !== ownedStore.id) {
    throw new Error(`task_create returned an unexpected qualified task: ${JSON.stringify(createdTask)}`);
  }

  const taskButton = page.getByRole("button", {
    name: new RegExp(`${task.id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")} .*${taskTitle.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`),
  });
  await taskButton.waitFor();
  const initialRead = await clickTaskAndRead(page, taskButton);
  if (initialRead.task.id !== task.id || !Array.isArray(initialRead.dependencies.dependencies)) {
    throw new Error(`Task detail calls returned unexpected data: ${JSON.stringify(initialRead)}`);
  }
  await page.getByLabel("Task status", { exact: true }).selectOption("in_progress");
  const updated = await actionResult(page, "task_update", () =>
    page.getByRole("button", { name: "Update status", exact: true }).click(),
  );
  if (updated?.task?.status !== "in_progress") {
    throw new Error(`task_update did not persist in_progress: ${JSON.stringify(updated)}`);
  }

  await clickTaskAndRead(page, taskButton);
  const closed = await actionResult(page, "task_close", () =>
    page.getByRole("button", { name: "Mark closed", exact: true }).click(),
  );
  if (closed?.task?.status !== "closed") {
    throw new Error(`task_close did not persist closed: ${JSON.stringify(closed)}`);
  }
  const closedRead = await clickTaskAndRead(page, taskButton);
  if (closedRead.task.status !== "closed") {
    throw new Error(`task_show did not observe the closed state: ${JSON.stringify(closedRead.task)}`);
  }
  await page.getByLabel("Task status", { exact: true }).selectOption("closed");
  await page.screenshot({ path: screenshots.task, fullPage: true });

  await actionResult(page, "mail_history", () =>
    page.getByRole("button", { name: "general", exact: true }).click(),
  );
  await page.getByLabel("Message kind", { exact: true }).selectOption("result");
  await page.getByLabel("Message", { exact: true }).fill(messageBody);
  await page.getByText("Add an evidence reference", { exact: true }).click();
  await page.getByLabel("Evidence reference URL", { exact: true }).fill(evidenceUrl);
  const sent = await actionResult(page, "mail_send", () =>
    page.getByRole("button", { name: "Send", exact: true }).click(),
  );
  if (
    sent?.message?.kind !== "result" ||
    sent.message.refs?.[0]?.url !== evidenceUrl ||
    sent.message.refs?.[0]?.label !== "Evidence"
  ) {
    throw new Error(`mail_send lost its annotation: ${JSON.stringify(sent)}`);
  }

  await page.getByRole("button", { name: "Ledger", exact: true }).click();
  await page.getByLabel("Filter ledger kinds", { exact: true }).waitFor();
  await page.locator("#details").getByText(messageBody, { exact: true }).waitFor();
  const evidenceLink = page.locator('#details a[href="https://example.com/orchard-live-smoke"]');
  await evidenceLink.waitFor();
  if ((await evidenceLink.getAttribute("rel")) !== "noreferrer") {
    throw new Error("Evidence link was not rendered with rel=noreferrer.");
  }

  if (pageErrors.length) {
    throw new Error(`Browser reported errors:\n${pageErrors.join("\n")}`);
  }
  console.log(
    JSON.stringify(
      {
        ok: true,
        server: origin.origin,
        workspace: { id: workspace.id, name: workspaceName },
        task: { store_id: ownedStore.id, task_id: task.id, status: closedRead.task.status },
        evidence: evidenceUrl,
        screenshots,
      },
      null,
      2,
    ),
  );
} catch (error) {
  await page.screenshot({ path: failureScreenshot, fullPage: true }).catch(() => {});
  console.error(
    JSON.stringify(
      {
        ok: false,
        error: error instanceof Error ? error.message : String(error),
        failureScreenshot,
      },
      null,
      2,
    ),
  );
  throw error;
} finally {
  await context.close();
  await browser.close();
}
