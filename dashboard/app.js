"use strict";

const state = {
  bridgeUrl: localStorage.getItem("structurely.bridgeUrl") || "http://127.0.0.1:4765",
  token: sessionStorage.getItem("structurely.token") || "",
  connected: false,
  view: "overview"
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
  memory: "/api/v1/memory"
};

const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];

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
    (url.hostname === "127.0.0.1" || url.hostname === "localhost" || url.hostname === "[::1]");
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
      referrerPolicy: "no-referrer"
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

function showView(view) {
  state.view = view;
  $$(".nav-item").forEach(item => item.classList.toggle("is-active", item.dataset.view === view));
  $$(".view").forEach(panel => panel.classList.toggle("is-visible", panel.dataset.viewPanel === view));
  const active = $(`.nav-item[data-view="${view}"]`);
  $("#view-name").textContent = active ? active.textContent.trim().replace(/^\d+/, "") : view;
  $(".sidebar").classList.remove("is-open");
  $("#mobile-menu").setAttribute("aria-expanded", "false");
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

function flattenResults(data) {
  if (Array.isArray(data)) return data;
  for (const key of ["results", "items", "symbols", "files", "sessions", "workspaces", "recaps", "memories", "path", "affected"]) {
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

function renderResults(container, data, label) {
  const items = flattenResults(data);
  if (!items.length) {
    container.innerHTML = emptyState(`No ${label.toLowerCase()} found`, "Try a more specific query or refresh the local index.");
    return;
  }
  container.innerHTML = `<div class="result-heading"><h2>${escapeHtml(label)}</h2><span>${items.length} item${items.length === 1 ? "" : "s"}</span></div><div class="result-list">${
    items.map((item, index) => `<article class="result-item">
      <div><h3>${escapeHtml(resultTitle(item, index))}</h3><p>${escapeHtml(item.file || item.symbol?.file || item.workspace_id || "")}</p></div>
      <p>${escapeHtml(resultCopy(item))}</p>
      <div class="meta">${escapeHtml(resultMeta(item))}</div>
    </article>`).join("")
  }</div>`;
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
  announce(`Running ${tool}`);
  try {
    const data = await bridgeRequest(endpoints[tool], { method: "POST", body: JSON.stringify(payload) });
    renderResults(container, data, tool === "memory" ? "Recalled memory" : tool === "recap" ? "Session recap" : `${tool[0].toUpperCase()}${tool.slice(1)} results`);
    announce(`${tool} complete`);
  } catch (error) {
    container.innerHTML = errorState(error);
    announce(`${tool} failed: ${error.message}`);
    if (error.status === 401) setConnection(false);
  }
}

async function loadCollection(name) {
  const container = $(`[data-collection="${name}"]`);
  if (!container) return;
  if (!state.token) {
    container.innerHTML = emptyState(`Connect to view ${name}`, "Pair this tab with the local bridge. Nothing is read from cloud storage.");
    return;
  }
  container.innerHTML = skeleton();
  try {
    const data = await bridgeRequest(endpoints[name], name === "sessions"
      ? { method: "POST", body: JSON.stringify({ limit: 20 }) }
      : {});
    renderResults(container, data, name[0].toUpperCase() + name.slice(1));
  } catch (error) {
    container.innerHTML = errorState(error);
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
    $("#health-content").innerHTML = `<p class="eyebrow">Bridge authenticated</p><h2><span>Healthy.</span> ${escapeHtml(root)}</h2>
      <div class="metric-grid">
        <div class="metric"><span>Index state</span><strong>${escapeHtml(status.state || status.status || "ready")}</strong></div>
        <div class="metric"><span>Files</span><strong>${escapeHtml(files)}</strong></div>
        <div class="metric"><span>Symbols</span><strong>${escapeHtml(symbols)}</strong></div>
        <div class="metric"><span>Relations</span><strong>${escapeHtml(relationships)}</strong></div>
      </div>`;
    loadActivity();
  } catch (error) {
    setConnection(false);
    if (error.status === 401) sessionStorage.removeItem("structurely.token");
  }
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
    errorElement.textContent = error.message;
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
  hydrateEmptyStates();
  $$(".nav-item").forEach(button => button.addEventListener("click", () => showView(button.dataset.view)));
  $$("[data-view-link]").forEach(button => button.addEventListener("click", () => showView(button.dataset.viewLink)));
  $$("[data-open-connect]").forEach(button => button.addEventListener("click", openConnection));
  $("#connection-button").addEventListener("click", openConnection);
  $("#connect-form").addEventListener("submit", pair);
  $$("[data-tool-form]").forEach(form => form.addEventListener("submit", event => {
    event.preventDefault();
    if (!state.token) return openConnection();
    runTool(form);
  }));
  $$("[data-refresh]").forEach(button => button.addEventListener("click", () => loadCollection(button.dataset.refresh)));
  $("#mobile-menu").addEventListener("click", event => {
    const open = $(".sidebar").classList.toggle("is-open");
    event.currentTarget.setAttribute("aria-expanded", String(open));
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
}

init();
