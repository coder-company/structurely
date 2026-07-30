const { test, expect } = require("@playwright/test");
const { pathToFileURL } = require("node:url");
const path = require("node:path");

const dashboardUrl = pathToFileURL(
  path.join(__dirname, "..", "dashboard", "index.html")
).href;

test.beforeEach(async ({ page }) => {
  await page.goto(dashboardUrl);
});

test("renders the supplied dashboard design tokens", async ({ page }) => {
  const body = page.locator("body");
  const primary = page.locator(".button.primary").first();
  const privacy = page.locator(".privacy-panel");

  await expect(page).toHaveTitle("Structurely Console");
  await expect(page.locator("#overview-title")).toBeVisible();
  await expect(body).toHaveCSS("background-color", "rgb(17, 19, 59)");
  await expect(body).toHaveCSS("font-weight", "300");
  await expect(primary).toHaveCSS("background-color", "rgb(102, 94, 253)");
  await expect(primary).toHaveCSS("border-radius", "9999px");
  await expect(privacy).toHaveCSS("background-color", "rgb(245, 233, 212)");
});

test("supports keyboard navigation and private pairing states", async ({ page }) => {
  await page.keyboard.press("Control+k");
  await expect(page.locator('[data-view-panel="search"]')).toBeVisible();
  await expect(page.locator('[data-view="search"]')).toHaveAttribute(
    "aria-current",
    "page"
  );
  await expect(page.locator("#search-query")).toBeFocused();

  await page.locator("#connection-button").click();
  const dialog = page.locator("#connect-dialog");
  await expect(dialog).toBeVisible();
  await expect(page.locator("#pair-code")).toBeFocused();
  await expect(page.locator("#bridge-url")).toHaveValue(
    "http://127.0.0.1:4765"
  );
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
});

test("collapses without horizontal overflow on a mobile viewport", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();

  const dimensions = await page.evaluate(() => ({
    viewport: document.documentElement.clientWidth,
    content: document.documentElement.scrollWidth,
  }));
  expect(dimensions.content).toBeLessThanOrEqual(dimensions.viewport);

  const menu = page.locator("#mobile-menu");
  await expect(menu).toBeVisible();
  await expect(menu).toHaveAttribute("aria-expanded", "false");
  await menu.click();
  await expect(menu).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator(".sidebar")).toHaveClass(/is-open/);
  await page.locator('[data-view="memory"]').click();
  await expect(menu).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator('[data-view-panel="memory"]')).toBeVisible();
});

test("honors reduced motion", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.reload();
  const duration = await page
    .locator(".view.is-visible")
    .evaluate((element) => getComputedStyle(element).animationDuration);
  expect(Number.parseFloat(duration)).toBeLessThanOrEqual(0.00001);
});
