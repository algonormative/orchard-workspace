import { expect, test } from "@playwright/test";

test.beforeEach(async ({ request }) => { await request.post("/fixture/reset"); });

async function unlock(page: import("@playwright/test").Page) {
  await page.goto("/");
  const accessKey = page.getByLabel("Local access key");
  if (await accessKey.isVisible()) {
    await accessKey.fill("fixture-access-key");
    await page.getByRole("button", { name: "Unlock" }).click();
  }
  await page.getByRole("heading", { name: "Start a workspace" }).or(page.getByLabel("Workspace", { exact: true })).waitFor();
  if (await page.getByLabel("Workspace name").isVisible()) {
    await page.getByLabel("Workspace name").fill("Fixture workspace");
    const created = page.waitForResponse((response) => response.request().postData()?.includes("workspace_create") || false);
    await page.getByRole("button", { name: "Create workspace" }).click();
    await created;
  }
  await expect(page.getByRole("button", { name: "general", exact: true })).toBeVisible();
}

test("responsive workspace keeps code contained and copies its exact text", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin: "http://127.0.0.1:4174" });
  await unlock(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const code = page.locator(".code-block code").first();
  await expect(code).not.toHaveText("Not loaded");
  const expected = await code.textContent();
  await page.locator(".code-block").first().getByRole("button", { name: "Copy", exact: true }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(expected);
  const geometry = await page.evaluate(() => ({ viewport: window.innerWidth, page: document.documentElement.scrollWidth, blocks: [...document.querySelectorAll<HTMLElement>(".code-block pre")].map((node) => ({ client: node.clientWidth, scroll: node.scrollWidth })) }));
  expect(geometry.page).toBeLessThanOrEqual(geometry.viewport);
  expect(geometry.blocks.some((block) => block.scroll >= block.client)).toBeTruthy();
  await page.getByRole("button", { name: "Back to workspace", exact: true }).first().click();
  await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin: "http://127.0.0.1:4174" });
  const fenced = page.locator("#thread .code-block").first();
  await expect(fenced).toBeVisible();
  await fenced.getByRole("button", { name: "Copy", exact: true }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe("printf 'fixture code'");
  await page.setViewportSize({ width: 1280, height: 800 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(1280);
});

test("first launch has an exitable calm home", async ({ page }) => {
  await page.goto("/");
  await page.getByLabel("Local access key").fill("fixture-access-key");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await page.getByRole("heading", { name: "Start a workspace", exact: true }).waitFor();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: "Orchard", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Create workspace", exact: true }).click();
  await expect(page.getByLabel("Workspace name", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("button", { name: "Create workspace", exact: true })).toBeVisible();
});

test("direct and broadcast routing, references, expiry, source errors, and archive remain observable", async ({ page, request }) => {
  await unlock(page);
  await page.getByRole("button", { name: "Alice", exact: true }).click();
  await expect(page.locator("#thread")).toContainText("Owner to Alice");
  await expect(page.locator("#thread")).not.toContainText("Agent to agent");
  await page.getByLabel("Message", { exact: true }).fill("Direct sent from browser fixture");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await page.getByRole("button", { name: "Broadcast", exact: true }).click();
  await page.getByLabel("Message", { exact: true }).fill("Broadcast sent from browser fixture");
  await page.getByText("Add an evidence reference", { exact: true }).click();
  await page.getByLabel("Evidence reference URL", { exact: true }).fill("https://example.com/evidence");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const audit = await (await request.get("/fixture/audit")).json();
  const sends = audit.calls.filter((entry: { operation: string }) => entry.operation === "mail_send");
  expect(sends.at(-2).args.destination).toEqual({ kind: "direct", id: "alice" });
  expect(sends.at(-1).args.destination).toEqual({ kind: "broadcast" });
  expect(sends.at(-1).args.refs[0].url).toBe("https://example.com/evidence");
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  await expect(page.locator('#details a[href="https://example.com/evidence"]')).toBeVisible();
  await request.post("/fixture/source-error");
  await page.waitForTimeout(8_500);
  await expect(page.locator("#conversations")).toContainText("Fixture backend is unavailable");
  await request.post("/fixture/revoke");
  await page.getByLabel("Message", { exact: true }).fill("Draft survives reconnect");
  await page.waitForTimeout(8_500);
  await expect(page.getByRole("heading", { name: "Unlock Orchard", exact: true })).toBeVisible();
  await page.getByLabel("Local access key").fill("fixture-access-key");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByLabel("Message", { exact: true })).toHaveValue("Draft survives reconnect");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("button", { name: "Archive workspace", exact: true }).click();
  await page.getByRole("button", { name: "Cancel archive", exact: true }).click();
  await expect(page.getByRole("button", { name: "Archive workspace", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Archive workspace", exact: true }).click();
  await page.getByRole("button", { name: "Confirm archive workspace", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Start a workspace", exact: true })).toBeVisible();
});

test("pending sends keep newer text and canceled delayed forms do not reopen", async ({ page, request }) => {
  await unlock(page);
  await request.post("/fixture/delay", { data: { send: 300, attach: 300 } });
  const composer = page.getByLabel("Message", { exact: true });
  await composer.fill("First submission");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await composer.fill("Newer unsent text");
  await expect(composer).toHaveValue("Newer unsent text");
  await page.waitForTimeout(450);
  await expect(composer).toHaveValue("Newer unsent text");
  await page.getByRole("button", { name: "Tasks", exact: true }).click();
  await page.getByRole("button", { name: "Add project", exact: true }).click();
  await page.getByLabel("Project path", { exact: true }).fill("/private/tmp/example-project");
  await page.getByRole("button", { name: "Add project", exact: true }).last().click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.waitForTimeout(450);
  await expect(page.getByRole("button", { name: "Add project", exact: true })).toBeVisible();
});

test("Activity search stays present across a poll", async ({ page }) => {
  await unlock(page);
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  const search = page.getByLabel("Search activity", { exact: true });
  await search.fill("agent");
  await search.focus();
  await page.waitForTimeout(8_500);
  await expect(search).toHaveValue("agent");
  await expect(search).toBeFocused();
});

test("exit paths, polling, and workspace task defaults preserve working context", async ({ page, request }) => {
  await unlock(page);
  await request.post("/fixture/long-history");
  await page.getByRole("button", { name: "general", exact: true }).click();
  await page.getByRole("button", { name: "Reply", exact: true }).first().click();
  const composer = page.getByLabel("Message", { exact: true });
  await composer.fill("Draft survives settings, cancel, and poll");
  await composer.focus();
  await page.locator("#thread").evaluate((thread) => { thread.scrollTop = 120; });
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.goBack();
  await expect(composer).toHaveValue("Draft survives settings, cancel, and poll");
  await expect(composer).toHaveAttribute("placeholder", "Write a reply");
  await expect.poll(() => page.locator("#thread").evaluate((thread) => thread.scrollTop)).toBeGreaterThan(0);

  await page.getByRole("button", { name: "Tasks", exact: true }).click();
  await expect(page.locator("#details")).toContainText("Workspace tasks");
  await expect(page.getByRole("button", { name: "New task", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Add project", exact: true }).click();
  const projectPath = page.getByLabel("Project path", { exact: true });
  await projectPath.fill("/private/tmp/example-project");
  await page.waitForTimeout(8_500);
  await expect(projectPath).toHaveValue("/private/tmp/example-project");
  await expect(projectPath).toBeFocused();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("button", { name: "Add project", exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Add project", exact: true }).click();
  await projectPath.fill("/private/tmp/example-project");
  await page.getByRole("button", { name: "Add project", exact: true }).last().click();
  await expect(page.locator("#details")).toContainText("example-project");
  await page.getByRole("button", { name: "Add project", exact: true }).click();
  await projectPath.fill("/private/tmp/plain-project");
  await page.getByRole("button", { name: "Add project", exact: true }).last().click();
  await expect(page.locator("#details")).toContainText("No project tasks found");

  await expect(page.getByLabel("Message kind", { exact: true })).toHaveCount(0);
  await composer.fill("Draft survives the eight-second poll");
  await composer.focus();
  await page.waitForTimeout(8_500);
  await expect(composer).toHaveValue("Draft survives the eight-second poll");
  await expect(composer).toBeFocused();

  await page.getByRole("button", { name: "New workspace", exact: true }).click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByLabel("Workspace", { exact: true })).toBeVisible();
  await expect(composer).toHaveValue("Draft survives the eight-second poll");
  await expect(composer).toHaveAttribute("placeholder", "Write a reply");
});
