"use strict";

const servedBridgeUrl = location.protocol === "http:" &&
  (location.hostname === "127.0.0.1" || location.hostname === "localhost")
  ? location.origin
  : "http://127.0.0.1:4765";

const state = {
  bridgeUrl: localStorage.getItem("structurely.bridgeUrl") || servedBridgeUrl,
  token: sessionStorage.getItem("structurely.token") || "",
  connected: false,
  view: "overview",
  theme: localStorage.getItem("structurely.theme") || "system"
};

const endpoints = {
  health: "/api/v1/health",
  pair: "/api/v1/pair",
  status: "/api/v1/status",
  search: "/api/v1/search",
  research: "/api/v1/research",
  impact: "/api/v1/impact",
  trace: "/api/v1/trace",
  workspaces: "/api/v1/workspaces",
  sessions: "/api/v1/sessions",
  recap: "/api/v1/recap",
  memory: "/api/v1/memory",
  createWorkspace: "/api/v1/workspaces",
  createSession: "/api/v1/sessions/create",
  appendEvent: "/api/v1/sessions/events",
  completeSession: "/api/v1/sessions/complete",
  remember: "/api/v1/memories"
};

const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];

const navigationSections = {
  analyze: [
    ["research", "Research"],
    ["impact", "Impact"],
    ["trace", "Path trace"]
  ],
  knowledge: [
    ["workspaces", "Workspaces"],
    ["sessions", "Sessions"],
    ["recaps", "Recaps"],
    ["memory", "Memory"]
  ]
};

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, character => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#039;"
  })[character]);
}

function announce(message) {
  $("#announcement").textContent = message;
}

function normalizeBridgeUrl(raw) {
  const url = new URL(raw);
  const allowed = url.protocol === "http:" &&
    (url.hostname === "127.0.0.1" || url.hostname === "localhost");
  if (!allowed || url.username || url.password || url.pathname !== "/" || url.search || url.hash) {
    throw new Error("Use a plain HTTP loopback address such as http://127.0.0.1:47831.");
  }
  return url.origin;
}

async function bridgeRequest(path, options = {}) {
  const headers = { "Accept": "application/json", ...(options.headers || {}) };
  if (options.body) headers["Content-Type"] = "application/json";
  if (state.token) headers.Authorization = `Bearer ${state.token}`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 15000);
  try {
    const response = await fetch(`${state.bridgeUrl}${path}`, {
      ...options,
      headers,
      signal: controller.signal,
      cache: "no-store",
      credentials: "omit",
      referrerPolicy: "no-referrer",
      targetAddressSpace: "loopback"
    });
    const data = await response.json().catch(() => ({}));
    if (!response.ok) {
      const error = new Error(data.error || data.message || `Bridge returned ${response.status}`);
      error.status = response.status;
      throw error;
    }
    return data;
  } catch (error) {
    if (error.name === "AbortError") throw new Error("The local bridge did not respond in time.");
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

function setConnection(connected) {
  state.connected = connected;
  $("#connection-button").classList.toggle("is-online", connected);
  $("#connection-label").textContent = connected ? "Local bridge online" : "Not connected";
}

function sectionForView(view) {
  return Object.entries(navigationSections)
    .find(([, destinations]) => destinations.some(([destination]) => destination === view))?.[0] || view;
}

function renderContextNavigation(view) {
  const section = sectionForView(view);
  const destinations = navigationSections[section] || [];
  const navigation = $("#context-navigation");
  navigation.innerHTML = destinations.map(([destination, label]) =>
    `<button type="button" class="context-item${destination === view ? " is-active" : ""}" data-context-view="${destination}"${destination === view ? ' aria-current="page"' : ""}>${label}</button>`
  ).join("");
  navigation.hidden = destinations.length === 0;
}

function showView(view) {
  const section = sectionForView(view);
  state.view = view;
  $$(".nav-item").forEach(item => {
    const isActive = item.dataset.view === view || item.dataset.section === section;
    item.classList.toggle("is-active", isActive);
    if (isActive) item.setAttribute("aria-current", "page");
    else item.removeAttribute("aria-current");
  });
  $$(".view").forEach(panel => panel.classList.toggle("is-visible", panel.dataset.viewPanel === view));
  const active = $(`.nav-item[data-view="${view}"]`);
  const contextLabel = navigationSections[section]?.find(([destination]) => destination === view)?.[1];
  $("#view-name").textContent = active ? active.textContent.trim().replace(/^\d+/, "") : contextLabel || view;
  renderContextNavigation(view);
  $(".sidebar").classList.remove("is-open");
  history.replaceState(null, "", `#${view}`);
  window.scrollTo({ top: 0, behavior: "smooth" });
  if (["workspaces", "sessions"].includes(view)) loadCollection(view);
}

function emptyState(title, copy) {
  return `<div class="result-empty"><div><span class="empty-mark" aria-hidden="true"></span><h2>${escapeHtml(title)}</h2><p>${escapeHtml(copy)}</p></div></div>`;
}

function errorState(error) {
  return `<div class="result-error" role="alert"><div><span class="empty-mark" aria-hidden="true"></span><h2>Could not load results</h2><p>${escapeHtml(error.message)} Check the bridge status and try again.</p></div></div>`;
}

function skeleton() {
  return `<div class="skeleton" aria-label="Loading results"><div class="skeleton-row"></div><div class="skeleton-row"></div><div class="skeleton-row"></div></div>`;
}

function setBusy(container, busy) {
  container.setAttribute("aria-busy", String(busy));
}

function flattenResults(data) {
  if (Array.isArray(data)) return data;
  for (const key of ["results", "items", "symbols", "symbol_findings", "content_findings", "files", "sessions", "workspaces", "recaps", "memories", "path", "steps", "affected"]) {
    if (Array.isArray(data?.[key])) return data[key];
  }
  return data && Object.keys(data).length ? [data] : [];
}

function resultTitle(item, index) {
  return item.name || item.title || item.symbol?.name || item.memory?.body || item.path || item.id || `Result ${index + 1}`;
}

function resultCopy(item) {
  return item.explanation || item.summary || item.body || item.snippet || item.content ||
    item.file || item.symbol?.file || item.memory?.body || "";
}

function resultMeta(item) {
  const parts = [
    item.kind || item.symbol?.kind || item.status,
    item.line ? `line ${item.line}` : "",
    typeof item.score === "number" ? `score ${item.score.toFixed(2)}` : ""
  ].filter(Boolean);
  return parts.join(" · ");
}

function formatScore(score) {
  return typeof score === "number" ? score.toFixed(2) : "";
}

function symbolPath(symbol) {
  if (!symbol?.file) return "";
  return symbol.start_line ? `${symbol.file}:${symbol.start_line}` : symbol.file;
}

function symbolActions(symbol) {
  if (!symbol?.name) return "";
  const name = escapeHtml(symbol.name);
  const file = escapeHtml(symbol.file || "");
  return `<div class="result-actions">
    <button type="button" class="text-button" data-analyze-symbol="${name}" data-symbol-file="${file}">Impact</button>
    <button type="button" class="text-button" data-trace-symbol="${name}" data-symbol-file="${file}">Trace from</button>
  </div>`;
}

function renderSymbolRow(hit) {
  const symbol = hit.symbol || hit;
  const source = hit.source || "";
  const relationshipCount = [hit.callers, hit.callees, hit.references, hit.referenced_by]
    .reduce((count, items) => count + (Array.isArray(items) ? items.length : 0), 0);
  const meta = [symbol.kind, symbol.language, symbol.start_line ? `line ${symbol.start_line}` : "",
    formatScore(hit.score) ? `score ${formatScore(hit.score)}` : "",
    relationshipCount ? `${relationshipCount} relationships` : ""].filter(Boolean).join(" · ");
  const disclosures = [
    hit.source_truncated ? "Source preview truncated to the response budget." : "",
    hit.relationships_truncated ? "Additional relationships were omitted by the response budget." : ""
  ].filter(Boolean);
  return `<article class="result-item result-symbol">
    <div><h3>${escapeHtml(symbol.qualified_name || symbol.name)}</h3><p class="file-reference">${escapeHtml(symbolPath(symbol))}</p></div>
    <div>${source ? `<pre class="source-preview"><code>${escapeHtml(source)}</code></pre>` : `<p>${escapeHtml(hit.explanation || "Indexed symbol")}</p>`}${disclosures.length ? `<p class="result-disclosure" role="note">${escapeHtml(disclosures.join(" "))}</p>` : ""}${symbolActions(symbol)}</div>
    <div class="meta">${escapeHtml(meta)}</div>
  </article>`;
}

function renderEvidence(evidence) {
  if (!evidence) return "";
  const location = evidence.file ? `${evidence.file}${evidence.line ? `:${evidence.line}` : ""}` : "";
  return `<div class="evidence"><p>${escapeHtml(evidence.explanation || evidence.provenance || "Relationship evidence")}</p><span>${escapeHtml([location, typeof evidence.confidence === "number" ? `${Math.round(evidence.confidence * 100)}% confidence` : ""].filter(Boolean).join(" · "))}</span></div>`;
}

function renderImpactResults(data) {
  const items = Array.isArray(data) ? data : [];
  if (!items.length) return emptyState("No downstream impact found", "The symbol may be isolated or unresolved. Add a file hint when names are ambiguous.");
  return resultFrame("Affected symbols", items.length, items.map(item => `<article class="result-item result-relationship">
    <div><h3>${escapeHtml(item.symbol?.qualified_name || item.symbol?.name)}</h3><p class="file-reference">${escapeHtml(symbolPath(item.symbol))}</p></div>
    <div>${renderEvidence(item.evidence)}${symbolActions(item.symbol)}</div>
    <div class="meta">depth ${escapeHtml(item.depth)}<br>${escapeHtml(item.origin?.name ? `from ${item.origin.name}` : "")}</div>
  </article>`).join(""));
}

function renderTraceResult(data) {
  const steps = Array.isArray(data?.path) ? data.path : [];
  if (!steps.length) {
    const candidates = [...(data?.source_candidates || []), ...(data?.target_candidates || [])];
    const candidateList = candidates.length ? `<div class="candidate-list">${candidates.map(renderSymbolRow).join("")}</div>` : "";
    return `<div class="result-empty result-guidance"><div><span class="empty-mark" aria-hidden="true"></span><h2>${escapeHtml(String(data?.status || "No path").replaceAll("_", " "))}</h2><p>${escapeHtml(data?.guidance || "No relationship path was found within the selected depth.")}</p></div>${candidateList}</div>`;
  }
  return `<div class="trace-summary"><strong>${steps.length} step${steps.length === 1 ? "" : "s"}</strong><span>${escapeHtml(data.examined_nodes)} nodes · ${escapeHtml(data.examined_edges)} edges examined</span></div><ol class="trace-list">${steps.map((step, index) => `<li>
    <div class="trace-index">${index + 1}</div>
    <article><div class="trace-symbols"><strong>${escapeHtml(step.source?.qualified_name || step.source?.name)}</strong><span>${escapeHtml(step.relationship)}</span><strong>${escapeHtml(step.target?.qualified_name || step.target?.name)}</strong></div>${renderEvidence(step.evidence)}${symbolActions(step.target)}</article>
  </li>`).join("")}</ol>`;
}

function renderResearchReport(data) {
  const symbols = Array.isArray(data?.symbol_findings) ? data.symbol_findings : [];
  const content = Array.isArray(data?.content_findings) ? data.content_findings : [];
  if (!symbols.length && !content.length) return emptyState("No evidence found", "Try a narrower question or refresh the local index.");
  const symbolSection = symbols.length ? `<section class="evidence-section"><div class="result-heading"><h2>Code evidence</h2><span>${symbols.length}</span></div><div class="result-list">${symbols.map(renderSymbolRow).join("")}</div></section>` : "";
  const contentSection = content.length ? `<section class="evidence-section"><div class="result-heading"><h2>Repository content</h2><span>${content.length}</span></div><div class="result-list">${content.map(hit => `<article class="result-item result-content"><div><h3>${escapeHtml(hit.title || hit.path)}</h3><p class="file-reference">${escapeHtml(hit.path)}:${escapeHtml(hit.start_line)}–${escapeHtml(hit.end_line)}</p></div><p>${escapeHtml(hit.text)}</p><div class="meta">score ${escapeHtml(formatScore(hit.score))}</div></article>`).join("")}</div></section>` : "";
  return `<div class="research-summary"><span>Graph epoch ${escapeHtml(data.graph_epoch)}</span><span>${(data.files || []).length} files consulted</span></div>${symbolSection}${contentSection}`;
}

function renderStateResults(items, label) {
  return resultFrame(label, items.length, items.map((item, index) => {
    const memory = item.memory || item;
    const isMemory = Boolean(item.memory || memory.tags);
    const title = isMemory ? memory.body : resultTitle(item, index);
    const copy = isMemory ? (memory.tags || []).join(" · ") : resultCopy(item);
    const timestamp = item.updated_at_ms || item.created_at_ms || item.started_at_ms;
    const sessionAction = item.status === "active" && item.id
      ? `<div class="result-actions"><button type="button" class="text-button" data-complete-session="${escapeHtml(item.id)}">Complete session</button></div>`
      : "";
    return `<article class="result-item result-state"><div><h3>${escapeHtml(title)}</h3><p>${escapeHtml(item.workspace_id || memory.workspace_id || "")}</p></div><div><p>${escapeHtml(copy)}</p>${sessionAction}</div><div class="meta">${escapeHtml(item.status || "")}${timestamp ? `<br>${escapeHtml(new Date(timestamp).toLocaleString())}` : ""}</div></article>`;
  }).join(""));
}

function resultFrame(label, count, content) {
  return `<div class="result-heading"><h2>${escapeHtml(label)}</h2><span>${count} item${count === 1 ? "" : "s"}</span></div><div class="result-list">${content}</div>`;
}

function renderResults(container, data, label, kind = "generic") {
  if (kind === "research") { container.innerHTML = renderResearchReport(data); return; }
  if (kind === "impact") { container.innerHTML = renderImpactResults(data); return; }
  if (kind === "trace") { container.innerHTML = renderTraceResult(data); return; }
  const items = flattenResults(data);
  if (!items.length) {
    container.innerHTML = emptyState(`No ${label.toLowerCase()} found`, "Try a more specific query or refresh the local index.");
    return;
  }
  if (["workspaces", "sessions", "memory", "recap"].includes(kind)) {
    container.innerHTML = renderStateResults(items, label);
    return;
  }
  container.innerHTML = resultFrame(label, items.length,
    items.map((item, index) => item.symbol ? renderSymbolRow(item) : `<article class="result-item">
      <div><h3>${escapeHtml(resultTitle(item, index))}</h3><p>${escapeHtml(item.file || item.symbol?.file || item.workspace_id || "")}</p></div>
      <p>${escapeHtml(resultCopy(item))}</p>
      <div class="meta">${escapeHtml(resultMeta(item))}</div>
    </article>`).join(""));
}

async function runTool(form) {
  const tool = form.dataset.toolForm;
  const container = tool === "memory" ? $('[data-collection="memory"]') : $(`[data-result="${tool}"]`);
  const payload = Object.fromEntries(new FormData(form).entries());
  for (const key of ["limit", "maxFiles", "depth"]) if (payload[key]) payload[key] = Number(payload[key]);
  if (payload.maxFiles) {
    payload.max_files = payload.maxFiles;
    delete payload.maxFiles;
  }
  for (const key of Object.keys(payload)) if (payload[key] === "") delete payload[key];
  container.innerHTML = skeleton();
  setBusy(container, true);
  announce(`Running ${tool}`);
  try {
    const data = await bridgeRequest(endpoints[tool], { method: "POST", body: JSON.stringify(payload) });
    renderResults(container, data, tool === "memory" ? "Recalled memory" : tool === "recap" ? "Session recap" : `${tool[0].toUpperCase()}${tool.slice(1)} results`, tool);
    announce(`${tool} complete`);
  } catch (error) {
    container.innerHTML = errorState(error);
    announce(`${tool} failed: ${error.message}`);
    if (error.status === 401) setConnection(false);
  } finally {
    setBusy(container, false);
  }
}

async function loadCollection(name) {
  const container = $(`[data-collection="${name}"]`);
  if (!container) return;
  if (!state.token) {
    container.innerHTML = emptyState(`Connect to view ${name}`, "Pair this tab with the local bridge. Nothing is read from cloud storage.");
    return;
  }
  if (name === "memory") {
    const form = $('[data-tool-form="memory"]');
    if (form?.checkValidity()) {
      await runTool(form);
    } else {
      container.innerHTML = emptyState("Enter a memory query", "Choose a workspace and describe what you want to recall.");
    }
    return;
  }
  container.innerHTML = skeleton();
  setBusy(container, true);
  try {
    const data = await bridgeRequest(endpoints[name], name === "sessions"
      ? { method: "POST", body: JSON.stringify({ limit: 20 }) }
      : {});
    renderResults(container, data, name[0].toUpperCase() + name.slice(1), name);
  } catch (error) {
    container.innerHTML = errorState(error);
  } finally {
    setBusy(container, false);
  }
}

async function runStateMutation(form) {
  if (!state.token) return openConnection();
  const action = form.dataset.stateForm;
  const configuration = {
    workspace: [endpoints.createWorkspace, "Workspace created", "workspaces"],
    session: [endpoints.createSession, "Session started", "sessions"],
    event: [endpoints.appendEvent, "Session event added", "sessions"],
    memory: [endpoints.remember, "Memory saved", "memory"]
  }[action];
  if (!configuration) return;
  const submit = $('button[type="submit"]', form);
  const originalLabel = submit.textContent;
  const payload = Object.fromEntries(new FormData(form).entries());
  if (action === "memory") payload.tags = payload.tags
    ? payload.tags.split(",").map(tag => tag.trim()).filter(Boolean)
    : [];
  submit.disabled = true;
  submit.textContent = "Saving…";
  announce(`Saving ${action}`);
  try {
    const result = await bridgeRequest(configuration[0], { method: "POST", body: JSON.stringify(payload) });
    form.reset();
    announce(configuration[1]);
    if (action === "workspace") {
      $("#session-workspace").value = result.id || "";
      $("#remember-workspace").value = result.id || "";
      $("#memory-workspace").value = result.id || "";
    } else if (action === "session") {
      $("#event-session").value = result.id || "";
      $("#recap-session").value = result.id || "";
    }
    await loadCollection(configuration[2]);
  } catch (error) {
    announce(`${action} failed: ${error.message}`);
    const target = configuration[2] === "memory"
      ? $('[data-collection="memory"]')
      : $(`[data-collection="${configuration[2]}"]`);
    if (target) target.innerHTML = errorState(error);
    if (error.status === 401) setConnection(false);
  } finally {
    submit.disabled = false;
    submit.textContent = originalLabel;
  }
}

async function completeSession(session) {
  announce("Completing session");
  try {
    await bridgeRequest(endpoints.completeSession, {
      method: "POST",
      body: JSON.stringify({ session })
    });
    announce("Session completed");
    await loadCollection("sessions");
  } catch (error) {
    $("[data-collection=\"sessions\"]").innerHTML = errorState(error);
    announce(`Session completion failed: ${error.message}`);
  }
}

function statusValue(status, keys, fallback = "—") {
  for (const key of keys) if (status?.[key] !== undefined) return status[key];
  return fallback;
}

async function refreshStatus() {
  if (!state.token) return;
  try {
    const status = await bridgeRequest(endpoints.status);
    setConnection(true);
    const root = status.project || status.root || status.project_path || "Active repository";
    const files = statusValue(status, ["files", "file_count", "indexed_files"]);
    const symbols = statusValue(status, ["symbols", "symbol_count"]);
    const relationships = statusValue(status, ["relationships", "relationship_count", "edges"]);
    const updated = status.updated_at || status.indexed_at || Date.now();
    $("#health-time").textContent = `Updated ${new Date(updated).toLocaleTimeString([], {hour: "2-digit", minute: "2-digit"})}`;
    $("#health-content").className = "health-online";
    const pending = Number(status.pending_files || 0);
    const skipped = Number(status.skipped_files || 0);
    const indexState = [status.state || status.status || "ready", pending ? `${pending} pending` : "", skipped ? `${skipped} skipped` : ""].filter(Boolean).join(" · ");
    $("#health-content").innerHTML = `<p class="eyebrow">Bridge authenticated</p><h2><span>Healthy.</span> ${escapeHtml(root)}</h2>
      <div class="metric-grid">
        <div class="metric"><span>Index state</span><strong>${escapeHtml(indexState)}</strong></div>
        <div class="metric"><span>Files</span><strong>${escapeHtml(files)}</strong></div>
        <div class="metric"><span>Symbols</span><strong>${escapeHtml(symbols)}</strong></div>
        <div class="metric"><span>Relations</span><strong>${escapeHtml(relationships)}</strong></div>
      </div>`;
    loadActivity();
  } catch (error) {
    setConnection(false);
    if (error.status === 401) {
      state.token = "";
      sessionStorage.removeItem("structurely.token");
    }
    renderConnectionIssue(error);
  }
}

function connectionMessage(error) {
  if (error?.status === 401) return ["Pairing expired", "The bridge no longer accepts this tab. Generate a new one-time code with structurely dashboard reconnect --path ."];
  if (error?.status === 409) return ["Pairing code already used", "Generate a fresh code with structurely dashboard reconnect --path ."];
  if (error?.status === 410) return ["Pairing code expired", "Generate a fresh code with structurely dashboard reconnect --path ."];
  if (error?.status === 429) return ["Bridge is protecting itself", "Wait a minute or run structurely dashboard reconnect --path . to rotate access."];
  return ["Local bridge unavailable", `Start or restart it with structurely dashboard serve --path . The console will reconnect to ${state.bridgeUrl}.`];
}

function renderConnectionIssue(error) {
  const [title, copy] = connectionMessage(error);
  $("#health-time").textContent = "Connection needed";
  $("#health-content").className = "health-empty connection-recovery";
  $("#health-content").innerHTML = `<div><h2>${escapeHtml(title)}</h2><p>${escapeHtml(copy)}</p></div><div class="recovery-actions"><button class="button primary" data-retry-connection>Retry</button><button class="button secondary" data-open-connect>Enter new code</button></div>`;
  announce(`${title}. ${copy}`);
}

async function loadActivity() {
  try {
    const data = await bridgeRequest(endpoints.sessions, {
      method: "POST",
      body: JSON.stringify({ limit: 4 })
    });
    const items = flattenResults(data);
    if (!items.length) return;
    $("#activity-list").innerHTML = `<div class="result-list">${items.map((item, index) =>
      `<article class="result-item"><div><h3>${escapeHtml(resultTitle(item, index))}</h3><p>${escapeHtml(item.status || "")}</p></div><p>${escapeHtml(item.summary || "")}</p><div class="meta">${escapeHtml(item.updated_at || "")}</div></article>`
    ).join("")}</div>`;
  } catch (_) {
    // Overview status remains useful when optional activity is unavailable.
  }
}

async function pair(event) {
  event.preventDefault();
  const form = event.currentTarget;
  const submit = $('button[type="submit"]', form);
  const errorElement = $("#connect-error");
  errorElement.textContent = "";
  submit.disabled = true;
  submit.textContent = "Pairing…";
  try {
    state.bridgeUrl = normalizeBridgeUrl(form.bridgeUrl.value);
    const data = await bridgeRequest(endpoints.pair, {
      method: "POST",
      body: JSON.stringify({ code: form.pairCode.value })
    });
    const token = data.token || data.access_token;
    if (!token) throw new Error("The bridge did not return an access token.");
    state.token = token;
    sessionStorage.setItem("structurely.token", token);
    localStorage.setItem("structurely.bridgeUrl", state.bridgeUrl);
    form.pairCode.value = "";
    $("#connect-dialog").close();
    announce("Local bridge connected");
    await refreshStatus();
  } catch (error) {
    const [title, recovery] = connectionMessage(error);
    errorElement.textContent = `${title}. ${recovery}`;
  } finally {
    submit.disabled = false;
    submit.textContent = "Pair this tab";
  }
}

function openConnection() {
  $("#bridge-url").value = state.bridgeUrl;
  $("#connect-error").textContent = "";
  $("#connect-dialog").showModal();
  setTimeout(() => $("#pair-code").focus(), 50);
}

function hydrateEmptyStates() {
  const copy = {
    search: ["Search the current index", "Enter a symbol, route, component, or file name to begin."],
    research: ["Ask a repository question", "Structurely will gather bounded evidence from symbols and file content."],
    impact: ["Plan a change with confidence", "Enter the symbol you may change to see affected code."],
    trace: ["Find a relationship path", "Choose a source and target symbol to trace their connection."],
    memory: ["Recall local knowledge", "Pair the bridge and search memories saved in this workspace."],
    recap: ["Generate a session recap", "Enter a session ID to summarize its local event history."]
  };
  for (const [name, content] of Object.entries(copy)) {
    const container = name === "memory" ? $('[data-collection="memory"]') : $(`[data-result="${name}"]`);
    container.innerHTML = emptyState(content[0], content[1]);
  }
  for (const name of ["workspaces", "sessions"]) {
    $(`[data-collection="${name}"]`).innerHTML = emptyState(`Connect to view ${name}`, "This data is held by your local Structurely state store.");
  }
}

function init() {
  applyTheme(state.theme);
  hydrateEmptyStates();
  $$(".nav-item[data-view]").forEach(button => button.addEventListener("click", () => showView(button.dataset.view)));
  $$("[data-section]").forEach(button => button.addEventListener("click", () => showView(navigationSections[button.dataset.section][0][0])));
  $$("[data-view-link]").forEach(button => button.addEventListener("click", () => showView(button.dataset.viewLink)));
  $("#connection-button").addEventListener("click", openConnection);
  $("#connect-form").addEventListener("submit", pair);
  $$("[data-tool-form]").forEach(form => form.addEventListener("submit", event => {
    event.preventDefault();
    if (!state.token) return openConnection();
    runTool(form);
  }));
  $$("[data-state-form]").forEach(form => form.addEventListener("submit", event => {
    event.preventDefault();
    runStateMutation(form);
  }));
  $$("[data-refresh]").forEach(button => button.addEventListener("click", () => loadCollection(button.dataset.refresh)));
  $("#theme-toggle").addEventListener("click", cycleTheme);
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (state.theme === "system") applyTheme("system");
  });
  document.addEventListener("click", event => {
    const completion = event.target.closest("[data-complete-session]");
    if (completion) {
      completeSession(completion.dataset.completeSession);
      return;
    }
    const context = event.target.closest("[data-context-view]");
    if (context) {
      showView(context.dataset.contextView);
      return;
    }
    if (event.target.closest("[data-retry-connection]")) {
      refreshStatus();
      return;
    }
    if (event.target.closest("[data-open-connect]")) {
      openConnection();
      return;
    }
    const analyze = event.target.closest("[data-analyze-symbol]");
    if (analyze) {
      showView("impact");
      $("#impact-symbol").value = analyze.dataset.analyzeSymbol;
      $("#impact-file").value = analyze.dataset.symbolFile || "";
      $("#impact-symbol").focus();
      return;
    }
    const trace = event.target.closest("[data-trace-symbol]");
    if (trace) {
      showView("trace");
      $("#trace-source").value = trace.dataset.traceSymbol;
      $("#trace-source").focus();
    }
  });
  $("#command-key").addEventListener("click", () => { showView("search"); setTimeout(() => $("#search-query").focus(), 50); });
  document.addEventListener("keydown", event => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      showView("search");
      $("#search-query").focus();
    }
  });
  const hashView = location.hash.slice(1);
  if ($(`[data-view-panel="${hashView}"]`)) showView(hashView);
  const fragment = new URLSearchParams(location.hash.slice(1));
  const pairingCode = fragment.get("pair");
  if (pairingCode) {
    history.replaceState(null, "", location.pathname + location.search);
    openConnection();
    $("#pair-code").value = pairingCode;
  } else if (state.token) {
    refreshStatus();
  }
  renderContextNavigation(state.view);
}

function applyTheme(theme) {
  const resolved = theme === "system"
    ? (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light")
    : theme;
  document.documentElement.dataset.theme = resolved;
  const button = $("#theme-toggle");
  if (button) {
    button.textContent = resolved === "dark" ? "Light" : "Dark";
    button.setAttribute("aria-label", `Switch to ${resolved === "dark" ? "light" : "dark"} theme`);
  }
}

function cycleTheme() {
  state.theme = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  localStorage.setItem("structurely.theme", state.theme);
  applyTheme(state.theme);
}

init();
