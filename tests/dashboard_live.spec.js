const { test, expect } = require("@playwright/test");
const { spawn, execFileSync } = require("node:child_process");
const { mkdtempSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const path = require("node:path");
const readline = require("node:readline");

let bridge;
let project;
let structurelyHome;
let ready;

test.beforeAll(async () => {
  const binary = path.resolve(
    process.env.STRUCTURELY_TEST_BINARY || "target/debug/structurely"
  );
  project = mkdtempSync(path.join(tmpdir(), "structurely-browser-live-"));
  structurelyHome = mkdtempSync(path.join(tmpdir(), "structurely-browser-home-"));
  writeFileSync(
    path.join(project, "main.rs"),
    "fn publish_atomically() {}\nfn main() { publish_atomically(); }\n"
  );
  execFileSync(binary, ["init", project], { stdio: "ignore" });
  execFileSync(binary, ["add", project], { stdio: "ignore", env: { ...process.env, STRUCTURELY_HOME: structurelyHome } });
  bridge = spawn(
    binary,
    ["dashboard", "start", "--port", "0"],
    { stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, STRUCTURELY_HOME: structurelyHome } }
  );
  ready = await new Promise((resolve, reject) => {
    const lines = readline.createInterface({ input: bridge.stdout });
    const timeout = setTimeout(
      () => reject(new Error("dashboard bridge did not become ready")),
      10000
    );
    bridge.once("exit", code => reject(new Error(`dashboard bridge exited ${code}`)));
    lines.once("line", line => {
      clearTimeout(timeout);
      lines.close();
      resolve(JSON.parse(line));
    });
  });
});

test.afterAll(async () => {
  if (bridge && bridge.exitCode === null) {
    bridge.kill("SIGTERM");
    await new Promise(resolve => bridge.once("exit", resolve));
  }
  if (project) rmSync(project, { recursive: true, force: true });
  if (structurelyHome) rmSync(structurelyHome, { recursive: true, force: true });
});

test("pairs and completes the evidence-to-memory workflow", async ({ page }) => {
  await page.goto(`http://${ready.address}`);
  await page.locator("#connection-button").click();
  await page.locator("#pair-code").fill(ready.pairing_code);
  await page.locator("#connect-form").evaluate(form => form.requestSubmit());
  await expect(page.locator("#connection-label")).toHaveText("Local bridge online");
  await expect(page.locator("#health-content")).toContainText("Symbols");
  await expect(page.locator("#health-content")).not.toContainText("—");

  await page.locator('[data-view="search"]').click();
  await page.locator("#search-query").fill("publish atomically");
  await page.locator('[data-tool-form="search"]').evaluate(form => form.requestSubmit());
  await expect(page.locator('[data-result="search"]')).toContainText("publish_atomically");
  await page.locator('[data-result="search"] [data-analyze-symbol]').first().click();
  await expect(page.locator("#impact-symbol")).toHaveValue("publish_atomically");

  await page.locator('[data-section="knowledge"]').click();
  await page.locator("#workspace-name").fill("Release engineering");
  await page.locator('[data-state-form="workspace"]').evaluate(form => form.requestSubmit());
  await expect(page.locator('[data-collection="workspaces"]')).toContainText("Release engineering");
  const workspace = await page.locator("#session-workspace").inputValue();
  expect(workspace).toMatch(/^ws_/);

  await page.locator('[data-context-view="sessions"]').click();
  await page.locator("#session-title").fill("Verify atomic publication");
  await page.locator('[data-state-form="session"]').evaluate(form => form.requestSubmit());
  await expect(page.locator('[data-collection="sessions"]')).toContainText("Verify atomic publication");
  const session = await page.locator("#event-session").inputValue();
  expect(session).toMatch(/^session_/);
  await page.locator("#event-body").fill("Keep rename and fsync in one seam.");
  await page.locator('[data-state-form="event"]').evaluate(form => form.requestSubmit());
  await page.locator(`[data-complete-session="${session}"]`).click();
  await expect(page.locator('[data-collection="sessions"]')).toContainText("completed");

  await page.locator('[data-context-view="memory"]').click();
  await page.locator("#remember-body").fill("Atomic publication is verified by rename and fsync.");
  await page.locator("#remember-tags").fill("storage, reliability");
  await page.locator('[data-state-form="memory"]').evaluate(form => form.requestSubmit());
  await page.locator("#memory-query").fill("rename fsync");
  await page.locator('[data-tool-form="memory"]').evaluate(form => form.requestSubmit());
  await expect(page.locator('[data-collection="memory"]')).toContainText(
    "Atomic publication is verified by rename and fsync."
  );
});
