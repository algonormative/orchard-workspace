import { expect, test } from "@playwright/test";

async function unlock(page: import("@playwright/test").Page) {
  await page.goto("/");
  const accessKey = page.getByLabel("Local access key");
  if (await accessKey.isVisible()) {
    await accessKey.fill("fixture-access-key");
    await page.getByRole("button", { name: "Unlock" }).click();
    await expect(page.getByRole("heading", { name: "Start a workspace" }).or(page.getByLabel("Workspace", { exact: true }))).toBeVisible();
  }
}

test("workspace conversations stay correct across polls and reconnect", async ({ page, request }) => {
  await unlock(page);
  if (await page.getByLabel("Workspace name").isVisible()) {
    await page.getByLabel("Workspace name").fill("Fixture workspace");
    await page.getByRole("button", { name: "Create workspace" }).click();
  }
  await expect(page.getByRole("button", { name: "general", exact: true })).toBeVisible();
  await expect(page.locator("#conversations")).not.toContainText("orchard-system");
  await page.getByRole("button", { name: "Tasks", exact: true }).click();
  const storeButton = page.locator("#details .store-button");
  await expect(storeButton).toBeVisible();
  const storeBounds = await storeButton.evaluate((node) => {
    const button = node.getBoundingClientRect();
    const details = node.closest("#details")!.getBoundingClientRect();
    return { right: button.right, detailsRight: details.right, scrollWidth: node.scrollWidth, clientWidth: node.clientWidth };
  });
  expect(storeBounds.right).toBeLessThanOrEqual(storeBounds.detailsRight + 0.5);
  expect(storeBounds.scrollWidth).toBeLessThanOrEqual(storeBounds.clientWidth);

  const composer = page.getByLabel("Message", { exact: true });
  await composer.fill("Draft survives the eight-second poll");
  await composer.focus();
  await page.waitForTimeout(8_500);
  await expect(composer).toHaveValue("Draft survives the eight-second poll");
  await expect(composer).toBeFocused();
  await composer.fill("General sent from browser fixture");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.locator("#thread")).toContainText("General sent from browser fixture");

  await page.getByRole("button", { name: "Alice", exact: true }).click();
  await expect(page.locator("#thread")).toContainText("Owner to Alice");
  await expect(page.locator("#thread")).toContainText("Alice to Owner");
  await expect(page.locator("#thread")).not.toContainText("Agent to agent");
  await page.getByLabel("Message", { exact: true }).fill("Direct sent from browser fixture");
  await page.getByRole("button", { name: "Send" }).click();

  await page.getByRole("button", { name: "All direct messages", exact: true }).click();
  await expect(page.locator("#thread")).toContainText("Agent to agent");
  await expect(page.locator("#thread")).toContainText("Alice → orchard");
  await expect(page.getByLabel("Message", { exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: "Broadcast", exact: true }).click();
  await page.getByLabel("Message", { exact: true }).fill("Broadcast sent from browser fixture");
  await page.getByRole("button", { name: "Send" }).click();
  const audit = await (await request.get("/fixture/audit")).json();
  const sends = audit.calls.filter((entry: { operation: string }) => entry.operation === "mail_send");
  expect(sends.at(-2).args.destination).toEqual({ kind: "direct", id: "alice" });
  expect(sends.at(-1).args.destination).toEqual({ kind: "broadcast" });
  expect(sends.at(-1).args.sender_id).toBe("owner");

  await page.getByRole("button", { name: "Ledger", exact: true }).click();
  await expect(page.locator("#details")).toContainText("Agent to agent");
  await request.post("/fixture/source-error");
  await page.waitForTimeout(8_500);
  await expect(page.locator("#conversations")).toContainText("Fixture backend is unavailable");
  await request.post("/fixture/task-backend-down");
  await page.waitForTimeout(8_500);
  await page.getByRole("button", { name: "Tasks", exact: true }).click();
  await expect(page.locator("#details")).toContainText("Beads backend did not start");

  await page.getByRole("button", { name: "Broadcast", exact: true }).click();
  await page.getByLabel("Message", { exact: true }).fill("Draft survives reconnect");
  await request.post("/fixture/revoke");
  await page.waitForTimeout(8_500);
  await expect(page.getByRole("heading", { name: "Unlock Orchard" })).toBeVisible();
  await page.getByLabel("Local access key").fill("fixture-access-key");
  await page.getByRole("button", { name: "Unlock" }).click();
  await expect(page.getByLabel("Message", { exact: true })).toHaveValue("Draft survives reconnect");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("button", { name: "Archive workspace", exact: true }).click();
  await page.getByRole("button", { name: "Confirm archive workspace", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Start a workspace" })).toBeVisible();
  await page.screenshot({ path: "test-results/workspace-fixture.png", fullPage: true });
});
