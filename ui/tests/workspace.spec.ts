import { expect, test, type Page } from "@playwright/test";

test.beforeEach(async ({ request }) => { await request.post("/fixture/reset"); });

async function unlock(page: Page, create = true) {
  await page.goto("/");
  const key = page.getByLabel("Local access key");
  if (await key.isVisible()) {
    await key.fill("fixture-access-key");
    await page.getByRole("button", { name: "Unlock", exact: true }).click();
  }
  await page.getByRole("heading", { name: "Start a workspace" }).or(page.getByLabel("Workspace", { exact: true })).waitFor();
  if (create && await page.getByLabel("Workspace name").isVisible()) {
    await page.getByLabel("Workspace name").fill("Fixture workspace");
    await page.getByRole("button", { name: "Create workspace", exact: true }).click();
  }
  if (create) await expect(page.getByLabel("Workspace", { exact: true })).toHaveValue("workspace-1");
}

async function openTree(page: Page, name: "Chats" | "Tasks" | "Artifacts") {
  const group = page.locator("details.tree-group").filter({ has: page.locator("summary", { hasText: name }) });
  if (!(await group.evaluate((node: HTMLDetailsElement) => node.open))) await group.locator("summary").click();
  return group;
}

test("first launch is calm and exitable", async ({ page }) => {
  await unlock(page, false);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: "Orchard", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Create workspace", exact: true }).click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("button", { name: "Create workspace", exact: true })).toBeVisible();
});

test("global tree opens chats, task defaults, artifacts, and tabs", async ({ page }) => {
  await unlock(page);
  const chats = await openTree(page, "Chats");
  await chats.getByRole("button", { name: "Alice", exact: true }).click();
  await expect(page.locator("#thread")).toContainText("Owner to Alice");
  await expect(page.locator("#thread")).not.toContainText("Agent to agent");
  await openTree(page, "Tasks");
  await page.getByRole("button", { name: "All tasks", exact: true }).click();
  await expect(page.locator("#conversation")).toContainText("Workspace tasks");
  await page.getByRole("button", { name: "Fixture task", exact: true }).first().click();
  await expect(page.getByRole("tab", { name: "Fixture task", exact: true })).toBeVisible();
  await openTree(page, "Artifacts");
  await page.getByRole("button", { name: /Fixture artifacts/ }).click();
  await page.getByRole("button", { name: "README.md", exact: true }).click();
  await expect(page.getByRole("tab", { name: "README.md", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Close README.md", exact: true }).click();
  await expect(page.getByRole("tab", { name: "Fixture task", exact: true })).toHaveAttribute("aria-selected", "true");
});

test("file viewer renders markdown, code, image, safe relative links, and pinned history", async ({ page }) => {
  await unlock(page); await openTree(page, "Artifacts");
  await page.getByRole("button", { name: /Fixture artifacts/ }).click();
  await page.getByRole("button", { name: "README.md", exact: true }).click();
  const viewer = page.locator("#conversation");
  await expect(viewer.getByRole("heading", { name: "Fixture heading" })).toBeVisible();
  await expect(viewer.locator(".code-block code")).toHaveText("console.log('fixture')");
  await expect(viewer.getByRole("button", { name: "Code", exact: true })).toBeVisible();
  await expect(viewer).toContainText("Outside");
  await expect(viewer.getByRole("button", { name: "Outside", exact: true })).toHaveCount(0);
  await viewer.getByText("Versions", { exact: true }).click();
  await viewer.getByRole("button", { name: /0123456789 Fixture version/ }).click();
  await expect(page).toHaveURL(/revision=0123456789abcdef0123456789abcdef01234567/);
  await viewer.getByRole("button", { name: "Code", exact: true }).click();
  await expect(page).toHaveURL(/path=docs%2Fexample.py&revision=0123456789abcdef0123456789abcdef01234567/);
  await expect(viewer.locator(".code-block code")).toHaveText("print('fixture')");
  await page.getByRole("button", { name: "image.png", exact: true }).click();
  await expect(viewer.getByRole("img", { name: "image.png" })).toHaveAttribute("src", "/fixture/image.png");
  await expect(viewer).not.toContainText("null");
});

test("typed attachments and resource links produce navigable backlinks", async ({ page, request }) => {
  await unlock(page); await openTree(page, "Artifacts");
  await page.getByRole("button", { name: /Fixture artifacts/ }).click();
  await page.getByRole("button", { name: "README.md", exact: true }).click();
  await page.getByRole("button", { name: "Add link", exact: true }).click();
  await page.getByLabel("Link resource").fill("/w/workspace-1/files/fixture-root?path=docs%2Fexample.py");
  await page.locator("form.inline-form").getByRole("button", { name: "Add link", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Links", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "docs/example.py", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Backlinks", exact: true })).toBeVisible();
  const chats = await openTree(page, "Chats");
  await chats.getByRole("button", { name: "general", exact: true }).click();
  await page.getByLabel("Message").fill("Typed attachment");
  await page.getByText("Attach", { exact: true }).click();
  await page.getByLabel("Attachment URL").fill("/w/workspace-1/files/fixture-root?path=README.md");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const audit = await (await request.get("/fixture/audit")).json();
  const sent = audit.calls.filter((entry: { operation: string }) => entry.operation === "mail_send").at(-1);
  expect(sent.args.refs).toEqual([{ type: "resource", resource: { kind: "file", workspace_id: "workspace-1", root_id: "fixture-root", path: "README.md" } }]);
  await expect(page.locator("#thread").getByRole("button", { name: "README.md", exact: true })).toBeVisible();
});

test("deep permalink survives login and second-workspace switching", async ({ page, request }) => {
  await unlock(page); await request.post("/fixture/revoke");
  await page.goto("/w/workspace-1/files/fixture-root?path=README.md");
  await page.getByLabel("Local access key").fill("fixture-access-key");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("tab", { name: "README.md", exact: true })).toBeVisible();
  await expect(page).toHaveURL(/\/w\/workspace-1\/files\/fixture-root\?path=README.md$/);
  await page.getByRole("button", { name: "New workspace", exact: true }).click();
  await page.getByLabel("Workspace name").fill("Second workspace");
  await page.getByRole("button", { name: "Create workspace", exact: true }).click();
  await expect(page.getByLabel("Workspace", { exact: true })).toHaveValue("workspace-2");
  await expect(page).toHaveURL(/\/w\/workspace-2\/channels\/general$/);
  await expect(page.getByRole("tab", { name: "README.md", exact: true })).toHaveCount(0);
  await page.getByLabel("Workspace", { exact: true }).selectOption("workspace-1");
  await expect(page.getByLabel("Workspace", { exact: true })).toHaveValue("workspace-1");
  await expect(page).toHaveURL(/\/w\/workspace-1\/channels\/general$/);
  await expect(page.locator("#thread")).toContainText("General fixture message");
  await expect(page.getByRole("tab", { name: "README.md", exact: true })).toHaveCount(0);
  await expect(page.locator("#conversation")).not.toContainText("Second workspace");
});

test("draft, reply, scroll, reconnect, and polling preserve working context", async ({ page, request }) => {
  await unlock(page); await request.post("/fixture/long-history");
  const chats = await openTree(page, "Chats"); await chats.getByRole("button", { name: "general", exact: true }).click();
  await page.getByRole("button", { name: "Reply", exact: true }).first().click();
  const composer = page.getByLabel("Message"); await composer.fill("Draft survives reconnect and poll");
  await page.locator("#thread").evaluate((thread) => { thread.scrollTop = 120; });
  await openTree(page, "Artifacts"); await page.getByRole("button", { name: /Fixture artifacts/ }).click();
  await page.getByRole("button", { name: "README.md", exact: true }).click();
  await page.waitForTimeout(8_500);
  await expect(page.getByRole("tab", { name: "README.md", exact: true })).toHaveAttribute("aria-selected", "true");
  await page.getByRole("button", { name: "Settings", exact: true }).click(); await page.goBack();
  await page.getByRole("tab", { name: /# general|general/i }).click();
  await expect(composer).toHaveValue("Draft survives reconnect and poll");
  await request.post("/fixture/revoke"); await page.waitForTimeout(8_500);
  await expect(page.getByRole("heading", { name: "Unlock Orchard" })).toBeVisible();
  await page.getByLabel("Local access key").fill("fixture-access-key"); await page.getByRole("button", { name: "Unlock" }).click();
  await expect(composer).toHaveValue("Draft survives reconnect and poll");
  await expect.poll(() => page.locator("#thread").evaluate((thread) => thread.scrollTop)).toBeGreaterThan(0);
  const tabs = page.getByRole("tab");
  while (await tabs.count()) await page.getByRole("button", { name: /^Close / }).last().click();
  await expect(page.locator("#conversation")).toContainText("Choose a resource");
});

test("pending operations preserve newer input and do not reopen stale views", async ({ page, request }) => {
  await unlock(page); await request.post("/fixture/delay", { data: { send: 300, attach: 300, action: 300 } });
  const chats = await openTree(page, "Chats"); await chats.getByRole("button", { name: "general", exact: true }).click();
  const composer = page.getByLabel("Message"); await composer.fill("First submission");
  await page.getByRole("button", { name: "Send", exact: true }).click(); await composer.fill("Newer unsent text");
  await page.waitForTimeout(450); await expect(composer).toHaveValue("Newer unsent text");
  await openTree(page, "Tasks"); await page.getByRole("button", { name: "Add project", exact: true }).click();
  await page.getByLabel("Project path").fill("/private/tmp/example-project");
  await page.getByRole("button", { name: "Add project", exact: true }).last().click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click(); await page.waitForTimeout(450);
  await expect(page.getByLabel("Project path")).toHaveCount(0);
  await page.getByRole("button", { name: "Fixture task", exact: true }).first().click();
  await page.getByLabel("Task status").selectOption("in_progress"); await page.getByRole("button", { name: "Update status" }).click();
  await page.getByRole("tab", { name: /general/i }).click(); await page.waitForTimeout(450);
  await expect(page.locator("#thread")).toBeVisible();
});

test("responsive code surfaces stay contained and copy exact text", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin: "http://127.0.0.1:4174" });
  await unlock(page); await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const code = page.locator(".code-block code").first(); await expect(code).not.toHaveText("Not loaded"); const expected = await code.textContent();
  await page.locator(".code-block").first().getByRole("button", { name: "Copy", exact: true }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(expected);
  const geometry = await page.evaluate(() => ({ viewport: innerWidth, page: document.documentElement.scrollWidth, blocks: [...document.querySelectorAll<HTMLElement>(".code-block pre")].map((node) => ({ client: node.clientWidth, scroll: node.scrollWidth })) }));
  expect(geometry.page).toBeLessThanOrEqual(geometry.viewport);
  expect(geometry.blocks.some((block) => block.scroll >= block.client)).toBeTruthy();
});
