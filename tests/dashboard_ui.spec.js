const { test, expect } = require("@playwright/test");
const AxeBuilder = require("@axe-core/playwright").default;
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
  const query = page.locator(".query-bar").first();

  await expect(page).toHaveTitle("Structurely Console");
  await expect(page.locator("#overview-title")).toBeVisible();
  await expect(body).toHaveCSS("background-color", "rgb(251, 250, 244)");
  await expect(body).toHaveCSS("font-weight", "400");
  await expect(primary).toHaveCSS("background-color", "rgb(32, 128, 141)");
  await expect(primary).toHaveCSS("border-radius", "8px");
  await expect(query).toHaveCSS("border-radius", "12px");
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
  await expect(page.locator("#connection-button")).toBeFocused();
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

  await expect(page.locator("#primary-navigation")).toBeVisible();
  await expect(page.locator("#primary-navigation .nav-item")).toHaveCount(4);
  await page.locator('[data-section="knowledge"]').click();
  await expect(page.locator("#context-navigation")).toBeVisible();
  await page.locator('[data-context-view="memory"]').click();
  await expect(page.locator('[data-view-panel="memory"]')).toBeVisible();
});

test("shows actionable connection recovery", async ({ page }) => {
  await expect(page.locator(".setup-steps li")).toHaveCount(2);
  await page.evaluate(() => renderConnectionIssue({ status: 401 }));
  await expect(page.locator("#health-content")).toContainText("Pairing expired");
  await expect(page.locator("#health-content")).toContainText("structurely dashboard reconnect --path .");
  await expect(page.locator("[data-retry-connection]")).toBeVisible();
  await expect(page.locator("[data-open-connect]")).toBeVisible();
});

test("supports light and dark themes", async ({ page }) => {
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.locator("#theme-toggle").click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator("body")).toHaveCSS("background-color", "rgb(9, 23, 23)");
});

test("renders evidence-rich result types and connects analysis flows", async ({ page }) => {
  await page.evaluate(() => {
    showView("search");
    renderResults(document.querySelector('[data-result="search"]'), [{
      symbol: { name: "publish", qualified_name: "store::publish", file: "src/store.rs", start_line: 42, kind: "function", language: "rust" },
      score: 17.25
    }], "Search results", "search");
  });
  await expect(page.locator('[data-result="search"] .file-reference')).toHaveText("src/store.rs:42");
  await expect(page.locator('[data-result="search"] .meta')).toContainText("score 17.25");
  await page.locator('[data-result="search"] [data-analyze-symbol]').click();
  await expect(page.locator('[data-view-panel="impact"]')).toBeVisible();
  await expect(page.locator("#impact-symbol")).toHaveValue("publish");
  await expect(page.locator("#impact-file")).toHaveValue("src/store.rs");

  await page.evaluate(() => {
    renderResults(document.querySelector('[data-result="research"]'), {
      graph_epoch: 7,
      files: ["src/store.rs", "README.md"],
      symbol_findings: [{ symbol: { name: "publish", file: "src/store.rs", start_line: 42, kind: "function", language: "rust" }, score: 12, source: "fn publish() {}" }],
      content_findings: [{ path: "README.md", title: "Atomic writes", text: "Writes publish with rename.", start_line: 10, end_line: 12, score: 8.5 }]
    }, "Research results", "research");
  });
  await expect(page.locator('[data-result="research"] .evidence-section')).toHaveCount(2);
  await expect(page.locator('[data-result="research"]')).toContainText("2 files consulted");

  await page.evaluate(() => {
    renderResults(document.querySelector('[data-result="trace"]'), {
      status: "found", examined_nodes: 3, examined_edges: 5,
      path: [{ source: { name: "start" }, target: { name: "finish", file: "src/main.rs" }, relationship: "calls", evidence: { explanation: "Direct call", file: "src/main.rs", line: 8, confidence: 1 } }]
    }, "Trace results", "trace");
  });
  await expect(page.locator('[data-result="trace"] .trace-list li')).toHaveCount(1);
  await expect(page.locator('[data-result="trace"]')).toContainText("100% confidence");
});

test("honors reduced motion", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.reload();
  const duration = await page
    .locator(".view.is-visible")
    .evaluate((element) => getComputedStyle(element).animationDuration);
  expect(Number.parseFloat(duration)).toBeLessThanOrEqual(0.00001);
});

test("meets automated WCAG checks on primary and evidence views", async ({ page }) => {
  const overview = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"])
    .analyze();
  expect(overview.violations).toEqual([]);

  await page.evaluate(() => {
    showView("research");
    renderResults(document.querySelector('[data-result="research"]'), {
      graph_epoch: 4,
      files: ["src/store.rs"],
      symbol_findings: [{
        symbol: { name: "publish", qualified_name: "store::publish", file: "src/store.rs", start_line: 42, kind: "function", language: "rust" },
        score: 9.2,
        source: "fn publish() {}",
        source_truncated: true,
        relationships_truncated: true
      }],
      content_findings: []
    }, "Research results", "research");
  });
  await page.waitForTimeout(300);
  await expect(page.locator(".result-disclosure")).toContainText("Source preview truncated");
  await expect(page.locator(".result-disclosure")).toContainText("relationships were omitted");
  const evidence = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"])
    .analyze();
  expect(evidence.violations).toEqual([]);
});
