//! Private dashboard bridge.
//!
//! The bridge is deliberately loopback-only. A deployed dashboard contains
//! static UI assets; every repository query is evaluated by this process.

use crate::{
    atomic_file, engine::PROJECT_DIR, state::StateStore, workflow::WorkflowService, Engine,
};
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs,
    io::{IsTerminal, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MAX_ORIGIN_BYTES: usize = 512;
const MAX_ALLOWED_ORIGINS: usize = 16;
const MAX_PAIR_FAILURES: u8 = 8;
const MAX_REQUESTS_PER_MINUTE: usize = 120;
const DEFAULT_PORT: u16 = 4765;
const DEFAULT_PROJECT_NAME: &str = "structurely-dashboard";
const STATE_FILE: &str = "dashboard.json";
const STOP_FILE: &str = "dashboard.stop";
const ROTATE_FILE: &str = "dashboard.rotate";

const INDEX_HTML: &[u8] = include_bytes!("../dashboard/index.html");
const APP_JS: &[u8] = include_bytes!("../dashboard/app.js");
const STYLES_CSS: &[u8] = include_bytes!("../dashboard/styles.css");
const CSP: &str = "default-src 'self'; connect-src http://127.0.0.1:* http://localhost:*; \
    img-src 'self' data:; style-src 'self'; script-src 'self'; object-src 'none'; \
    base-uri 'none'; frame-ancestors 'none'; form-action 'none'";

#[derive(Debug, Clone)]
pub struct BridgeOptions {
    pub project: PathBuf,
    pub port: u16,
    pub allowed_origins: Vec<String>,
}

impl BridgeOptions {
    pub fn new(project: impl Into<PathBuf>) -> Self {
        Self {
            project: project.into(),
            port: DEFAULT_PORT,
            allowed_origins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BridgeReady {
    pub address: SocketAddr,
    pub pairing_code: String,
    pub project: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardStatus {
    pub running: bool,
    pub pid: u32,
    pub address: SocketAddr,
    pub project: String,
    pub allowed_origins: Vec<String>,
    pub pairing_code: Option<String>,
    pub generation: u64,
    pub started_unix_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardStop {
    pub stopped: bool,
    pub removed_state: bool,
}

pub fn offer_after_setup(project: &std::path::Path) {
    let selection = match std::env::var("STRUCTURELY_DASHBOARD_SETUP") {
        Ok(value) if value != "prompt" => Some(value),
        Ok(_) => prompt_dashboard_selection(),
        Err(_) if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() => {
            prompt_dashboard_selection()
        }
        Err(_) => None,
    };
    let Some(selection) = selection else {
        return;
    };
    match selection.trim().to_ascii_lowercase().as_str() {
        "" | "skip" | "4" => {}
        "local" | "local-only" | "3" => {
            eprintln!(
                "\nPrivate dashboard (local only):\n  structurely dashboard serve --path \"{}\"\n",
                project.display()
            );
        }
        "vercel" | "1" => offer_deployment("vercel", project),
        "cloudflare" | "2" => offer_deployment("cloudflare", project),
        other => eprintln!(
            "Dashboard setup choice {other:?} is invalid; use vercel, cloudflare, local, or skip."
        ),
    }
}

fn prompt_dashboard_selection() -> Option<String> {
    eprintln!(
        "\nDeploy your private dashboard shell?\n  1. Vercel\n  2. Cloudflare Pages\n  \
         3. Local only\n  4. Skip\n\nRepository data always stays in the local bridge."
    );
    eprint!("Choice [4]: ");
    let _ = std::io::stderr().flush();
    let mut choice = String::new();
    match std::io::stdin().read_line(&mut choice) {
        Ok(_) => Some(choice),
        Err(error) => {
            eprintln!("Could not read dashboard choice: {error}");
            None
        }
    }
}

fn offer_deployment(provider: &str, project: &std::path::Path) {
    match deploy(provider, None) {
        Ok(report) => eprintln!(
            "\nDashboard deployed: {}\nStart the private bridge:\n  structurely dashboard serve \
             --path \"{}\" --allow-origin \"{}\"\n",
            report.url,
            project.display(),
            report.url
        ),
        Err(error) => eprintln!(
            "\nStructurely setup succeeded, but optional {provider} dashboard deployment failed: \
             {error:#}\nInstall and authenticate the provider CLI, then retry:\n  structurely \
             dashboard deploy {provider}\n"
        ),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardExport {
    pub directory: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardDeployment {
    pub provider: String,
    pub project: String,
    pub url: String,
    pub verified: bool,
    pub data_uploaded: bool,
}

pub fn export(destination: impl Into<PathBuf>) -> Result<DashboardExport> {
    let destination = destination.into();
    if destination.exists() {
        anyhow::ensure!(
            destination.is_dir(),
            "dashboard export destination is not a directory: {}",
            destination.display()
        );
        anyhow::ensure!(
            fs::read_dir(&destination)?.next().is_none(),
            "dashboard export destination must be empty: {}",
            destination.display()
        );
    } else {
        fs::create_dir_all(&destination).with_context(|| {
            format!(
                "create dashboard export directory {}",
                destination.display()
            )
        })?;
    }
    let files = [
        ("index.html", INDEX_HTML),
        ("app.js", APP_JS),
        ("styles.css", STYLES_CSS),
    ];
    for (name, contents) in files {
        fs::write(destination.join(name), contents)
            .with_context(|| format!("write dashboard asset {name}"))?;
    }
    let cloudflare_headers = format!(
        "/*\n  Content-Security-Policy: {CSP}\n  Referrer-Policy: no-referrer\n  \
         X-Content-Type-Options: nosniff\n  X-Frame-Options: DENY\n  \
         Permissions-Policy: camera=(), microphone=(), geolocation=()\n"
    );
    fs::write(destination.join("_headers"), cloudflare_headers)?;
    let vercel = serde_json::json!({
        "headers": [{
            "source": "/(.*)",
            "headers": [
                {"key": "Content-Security-Policy", "value": CSP},
                {"key": "Referrer-Policy", "value": "no-referrer"},
                {"key": "X-Content-Type-Options", "value": "nosniff"},
                {"key": "X-Frame-Options", "value": "DENY"},
                {"key": "Permissions-Policy", "value": "camera=(), microphone=(), geolocation=()"}
            ]
        }]
    });
    fs::write(
        destination.join("vercel.json"),
        serde_json::to_vec_pretty(&vercel)?,
    )?;
    Ok(DashboardExport {
        directory: destination.display().to_string(),
        files: vec![
            "index.html".to_owned(),
            "app.js".to_owned(),
            "styles.css".to_owned(),
            "_headers".to_owned(),
            "vercel.json".to_owned(),
        ],
    })
}

pub fn deploy(provider: &str, project_name: Option<&str>) -> Result<DashboardDeployment> {
    let provider = provider.trim().to_ascii_lowercase();
    anyhow::ensure!(
        matches!(provider.as_str(), "vercel" | "cloudflare"),
        "dashboard provider must be vercel or cloudflare"
    );
    let project = project_name.unwrap_or(DEFAULT_PROJECT_NAME);
    validate_project_name(project)?;
    let cli = if provider == "vercel" {
        "vercel"
    } else {
        "wrangler"
    };
    require_command(cli, &["--version"])?;
    require_command("curl", &["--version"])?;

    let temporary = temporary_deploy_directory()?;
    let cleanup = TemporaryDirectory(temporary.clone());
    export(&temporary)?;
    let mut command = Command::new(cli);
    if provider == "vercel" {
        command
            .arg("deploy")
            .arg(&temporary)
            .arg("--prod")
            .arg("--yes")
            .arg("--project")
            .arg(project);
    } else {
        command
            .arg("pages")
            .arg("deploy")
            .arg(&temporary)
            .arg("--project-name")
            .arg(project)
            .arg("--branch")
            .arg("main");
    }
    let output = command
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("run {provider} dashboard deployment"))?;
    anyhow::ensure!(
        output.status.success(),
        "{provider} dashboard deployment failed with {}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).context("provider output was not UTF-8")?;
    let url = deployment_url(&stdout).context(
        "provider reported success without a deployment URL; inspect the provider dashboard",
    )?;
    let verified = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            "20",
            &url,
        ])
        .stdout(Stdio::null())
        .status()
        .context("verify deployed dashboard")?
        .success();
    anyhow::ensure!(
        verified,
        "dashboard deployed but did not pass HTTP verification: {url}"
    );
    drop(cleanup);
    Ok(DashboardDeployment {
        provider,
        project: project.to_owned(),
        url,
        verified,
        data_uploaded: false,
    })
}

pub fn status(project: impl Into<PathBuf>) -> Result<Option<DashboardStatus>> {
    let project = project.into();
    let project = project
        .canonicalize()
        .with_context(|| format!("resolve dashboard project {}", project.display()))?;
    let path = project.join(PROJECT_DIR).join(STATE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read dashboard state"),
    };
    let mut state: DashboardStatus =
        serde_json::from_slice(&bytes).context("parse dashboard state")?;
    state.running = bridge_health(state.address);
    Ok(Some(state))
}

pub fn rotate(project: impl Into<PathBuf>) -> Result<DashboardStatus> {
    let project = project.into().canonicalize()?;
    let current = status(&project)?.context("dashboard bridge is not running")?;
    anyhow::ensure!(current.running, "dashboard bridge is not running");
    let rotate = project.join(PROJECT_DIR).join(ROTATE_FILE);
    fs::write(&rotate, b"rotate\n").context("request dashboard token rotation")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(next) = status(&project)? {
            if next.running && next.generation > current.generation {
                return Ok(next);
            }
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "dashboard bridge did not rotate its pairing token within 5 seconds"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn stop(project: impl Into<PathBuf>) -> Result<DashboardStop> {
    let project = project.into().canonicalize()?;
    let Some(current) = status(&project)? else {
        return Ok(DashboardStop {
            stopped: false,
            removed_state: false,
        });
    };
    if !current.running {
        let removed_state = remove_if_present(&project.join(PROJECT_DIR).join(STATE_FILE)).is_ok();
        return Ok(DashboardStop {
            stopped: false,
            removed_state,
        });
    }
    fs::write(project.join(PROJECT_DIR).join(STOP_FILE), b"stop\n")
        .context("request dashboard bridge stop")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if status(&project)?.is_none() {
            return Ok(DashboardStop {
                stopped: true,
                removed_state: true,
            });
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "dashboard bridge did not stop within 5 seconds"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn remove(project: impl Into<PathBuf>) -> Result<DashboardStop> {
    let project = project.into().canonicalize()?;
    let mut report = stop(&project)?;
    for name in [STATE_FILE, STOP_FILE, ROTATE_FILE] {
        remove_if_present(&project.join(PROJECT_DIR).join(name))?;
    }
    report.removed_state = true;
    Ok(report)
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Bridge {
    project: PathBuf,
    pairing_code: Mutex<Option<String>>,
    token: Mutex<String>,
    allowed_origins: Vec<String>,
    state_path: PathBuf,
    address: SocketAddr,
    generation: Mutex<u64>,
    started_unix_ms: u128,
    failed_pair_attempts: Mutex<u8>,
    request_times: Mutex<VecDeque<Instant>>,
}

#[derive(Debug, Deserialize)]
struct PairRequest {
    code: String,
}

#[derive(Debug, Deserialize)]
struct SearchRequest {
    query: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct ResearchRequest {
    query: String,
    #[serde(default = "default_research_files")]
    max_files: usize,
}

#[derive(Debug, Deserialize)]
struct ImpactRequest {
    symbol: String,
    file: Option<String>,
    #[serde(default = "default_depth")]
    depth: usize,
}

#[derive(Debug, Deserialize)]
struct TraceRequest {
    source: String,
    target: String,
    source_file: Option<String>,
    target_file: Option<String>,
    #[serde(default = "default_trace_depth")]
    depth: usize,
}

#[derive(Debug, Deserialize)]
struct SessionsRequest {
    workspace: Option<String>,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct MemoryRequest {
    workspace: String,
    query: String,
    #[serde(default = "default_memory_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct RecapRequest {
    session: String,
}

fn default_search_limit() -> usize {
    20
}

fn default_research_files() -> usize {
    12
}

fn default_memory_limit() -> usize {
    10
}

fn default_depth() -> usize {
    2
}

fn default_trace_depth() -> usize {
    6
}

pub fn serve(options: BridgeOptions) -> Result<BridgeReady> {
    let project = options
        .project
        .canonicalize()
        .with_context(|| format!("resolve dashboard project {}", options.project.display()))?;
    // Fail before opening a listener if the project is not initialized.
    Engine::open_read_only(&project)?;
    let mut allowed_origins = validate_origins(&options.allowed_origins)?;
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), options.port);
    let server = Server::http(address)
        .map_err(|error| anyhow::anyhow!("bind dashboard bridge at {address}: {error}"))?;
    let address = server
        .server_addr()
        .to_ip()
        .context("dashboard bridge did not bind an IP socket")?;
    anyhow::ensure!(
        address.ip().is_loopback(),
        "dashboard bridge refused a non-loopback address"
    );
    allowed_origins.push(format!("http://127.0.0.1:{}", address.port()));
    allowed_origins.push(format!("http://localhost:{}", address.port()));
    allowed_origins.sort();
    allowed_origins.dedup();

    let pairing_code = random_decimal_code()?;
    let dashboard_directory = project.join(PROJECT_DIR);
    let state_path = dashboard_directory.join(STATE_FILE);
    let stop_path = dashboard_directory.join(STOP_FILE);
    let rotate_path = dashboard_directory.join(ROTATE_FILE);
    let _ = fs::remove_file(&stop_path);
    let _ = fs::remove_file(&rotate_path);
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let bridge = Bridge {
        project: project.clone(),
        pairing_code: Mutex::new(Some(pairing_code.clone())),
        token: Mutex::new(random_token()?),
        allowed_origins: allowed_origins.clone(),
        state_path: state_path.clone(),
        address,
        generation: Mutex::new(1),
        started_unix_ms,
        failed_pair_attempts: Mutex::new(0),
        request_times: Mutex::new(VecDeque::new()),
    };
    bridge.write_state()?;
    let ready = BridgeReady {
        address,
        pairing_code,
        project: project.display().to_string(),
    };
    println!("{}", serde_json::to_string(&ready)?);
    std::io::stdout().flush()?;

    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))
        .context("install dashboard shutdown handler")?;
    while !stop.load(Ordering::Relaxed) {
        if stop_path.exists() {
            stop.store(true, Ordering::Relaxed);
            break;
        }
        if rotate_path.exists() {
            let _ = fs::remove_file(&rotate_path);
            bridge.rotate()?;
        }
        if let Some(request) = server
            .recv_timeout(Duration::from_millis(100))
            .context("receive dashboard request")?
        {
            bridge.handle(request);
        }
    }
    let _ = fs::remove_file(&state_path);
    let _ = fs::remove_file(&stop_path);
    let _ = fs::remove_file(&rotate_path);
    Ok(ready)
}

impl Bridge {
    fn handle(&self, mut request: Request) {
        let origin = header(&request, "Origin").map(str::to_owned);
        if request.method() == &Method::Options {
            let allowed = origin
                .as_deref()
                .is_some_and(|origin| self.origin_allowed(origin));
            let response = if allowed {
                empty_response(StatusCode(204), origin.as_deref(), true)
            } else {
                json_error(StatusCode(403), "origin is not allowed", None)
            };
            let _ = request.respond(response);
            return;
        }

        let path = request.url().split('?').next().unwrap_or(request.url());
        if !self.request_origin_allowed(origin.as_deref()) {
            let _ = request.respond(json_error(
                StatusCode(403),
                "origin is not allowed",
                origin.as_deref(),
            ));
            return;
        }

        let response = match (request.method(), path) {
            (&Method::Get, "/") | (&Method::Get, "/index.html") => {
                asset_response(INDEX_HTML, "text/html; charset=utf-8")
            }
            (&Method::Get, "/app.js") => asset_response(APP_JS, "text/javascript; charset=utf-8"),
            (&Method::Get, "/styles.css") => asset_response(STYLES_CSS, "text/css; charset=utf-8"),
            (&Method::Get, "/api/v1/health") => json_response(
                StatusCode(200),
                &serde_json::json!({"ready": true}),
                origin.as_deref(),
            ),
            (&Method::Post, "/api/v1/pair") => self.pair(&mut request, origin.as_deref()),
            _ if !self.authorized(&request) => {
                json_error(StatusCode(401), "pairing is required", origin.as_deref())
            }
            _ if !self.within_rate_limit() => json_error(
                StatusCode(429),
                "dashboard request rate exceeded",
                origin.as_deref(),
            ),
            (&Method::Get, "/api/v1/status") => self.engine_response(origin.as_deref(), |engine| {
                serde_json::to_value(engine.status()?).map_err(Into::into)
            }),
            (&Method::Post, "/api/v1/search") => {
                self.with_json::<SearchRequest, _>(&mut request, origin.as_deref(), |body| {
                    let engine = Engine::open_read_only(&self.project)?;
                    Ok(serde_json::to_value(
                        engine.search(&body.query, body.limit)?,
                    )?)
                })
            }
            (&Method::Post, "/api/v1/research") => {
                self.with_json::<ResearchRequest, _>(&mut request, origin.as_deref(), |body| {
                    let engine = Engine::open_read_only(&self.project)?;
                    Ok(serde_json::to_value(
                        WorkflowService::new(&engine).research(&body.query, body.max_files)?,
                    )?)
                })
            }
            (&Method::Post, "/api/v1/impact") => {
                self.with_json::<ImpactRequest, _>(&mut request, origin.as_deref(), |body| {
                    let engine = Engine::open_read_only(&self.project)?;
                    Ok(serde_json::to_value(engine.impact_named(
                        &body.symbol,
                        body.file.as_deref(),
                        body.depth,
                    )?)?)
                })
            }
            (&Method::Post, "/api/v1/trace") => {
                self.with_json::<TraceRequest, _>(&mut request, origin.as_deref(), |body| {
                    let engine = Engine::open_read_only(&self.project)?;
                    Ok(serde_json::to_value(engine.trace_path_named(
                        &body.source,
                        body.source_file.as_deref(),
                        &body.target,
                        body.target_file.as_deref(),
                        body.depth,
                    )?)?)
                })
            }
            (&Method::Get, "/api/v1/workspaces") => self
                .state_response(origin.as_deref(), |state| {
                    Ok(serde_json::to_value(state.list_workspaces(100)?)?)
                }),
            (&Method::Post, "/api/v1/sessions") => {
                self.with_json::<SessionsRequest, _>(&mut request, origin.as_deref(), |body| {
                    let state = StateStore::open(&self.project)?;
                    Ok(serde_json::to_value(
                        state.list_sessions(body.workspace.as_deref(), body.limit)?,
                    )?)
                })
            }
            (&Method::Post, "/api/v1/memory") => {
                self.with_json::<MemoryRequest, _>(&mut request, origin.as_deref(), |body| {
                    let state = StateStore::open(&self.project)?;
                    Ok(serde_json::to_value(state.search_memories(
                        &body.workspace,
                        &body.query,
                        body.limit,
                    )?)?)
                })
            }
            (&Method::Post, "/api/v1/recap") => {
                self.with_json::<RecapRequest, _>(&mut request, origin.as_deref(), |body| {
                    let state = StateStore::open(&self.project)?;
                    Ok(serde_json::to_value(state.generate_recap(&body.session)?)?)
                })
            }
            _ => json_error(StatusCode(404), "route not found", origin.as_deref()),
        };
        let _ = request.respond(response);
    }

    fn pair(&self, request: &mut Request, origin: Option<&str>) -> BridgeResponse {
        let body = match read_json::<PairRequest>(request) {
            Ok(body) => body,
            Err(error) => return json_error(StatusCode(400), &error.to_string(), origin),
        };
        let mut pairing = match self.pairing_code.lock() {
            Ok(pairing) => pairing,
            Err(_) => return json_error(StatusCode(500), "pairing state is unavailable", origin),
        };
        let Some(expected) = pairing.as_deref() else {
            return json_error(StatusCode(409), "pairing code was already used", origin);
        };
        let mut failures = match self.failed_pair_attempts.lock() {
            Ok(failures) => failures,
            Err(_) => return json_error(StatusCode(500), "pairing state is unavailable", origin),
        };
        if *failures >= MAX_PAIR_FAILURES {
            return json_error(
                StatusCode(429),
                "too many invalid pairing attempts; rotate the token locally",
                origin,
            );
        }
        if !constant_time_eq(expected.as_bytes(), body.code.as_bytes()) {
            *failures = failures.saturating_add(1);
            return json_error(StatusCode(401), "pairing code is invalid", origin);
        }
        *failures = 0;
        drop(failures);
        *pairing = None;
        drop(pairing);
        if self.write_state().is_err() {
            return json_error(
                StatusCode(500),
                "pairing state could not be persisted",
                origin,
            );
        }
        let token = match self.token.lock() {
            Ok(token) => token.clone(),
            Err(_) => return json_error(StatusCode(500), "pairing state is unavailable", origin),
        };
        json_response(
            StatusCode(200),
            &serde_json::json!({"token": token}),
            origin,
        )
    }

    fn authorized(&self, request: &Request) -> bool {
        let Some(value) = header(request, "Authorization") else {
            return false;
        };
        let Some(token) = value.strip_prefix("Bearer ") else {
            return false;
        };
        self.token
            .lock()
            .is_ok_and(|expected| constant_time_eq(token.as_bytes(), expected.as_bytes()))
    }

    fn request_origin_allowed(&self, origin: Option<&str>) -> bool {
        origin.is_none_or(|origin| self.origin_allowed(origin))
    }

    fn within_rate_limit(&self) -> bool {
        let Ok(mut requests) = self.request_times.lock() else {
            return false;
        };
        let cutoff = Instant::now() - Duration::from_secs(60);
        while requests.front().is_some_and(|request| *request < cutoff) {
            requests.pop_front();
        }
        if requests.len() >= MAX_REQUESTS_PER_MINUTE {
            return false;
        }
        requests.push_back(Instant::now());
        true
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        self.allowed_origins
            .iter()
            .any(|allowed| constant_time_eq(allowed.as_bytes(), origin.as_bytes()))
    }

    fn with_json<T, F>(
        &self,
        request: &mut Request,
        origin: Option<&str>,
        operation: F,
    ) -> BridgeResponse
    where
        T: DeserializeOwned,
        F: FnOnce(T) -> Result<serde_json::Value>,
    {
        let body = match read_json(request) {
            Ok(body) => body,
            Err(error) => return json_error(StatusCode(400), &error.to_string(), origin),
        };
        match operation(body) {
            Ok(value) => json_response(StatusCode(200), &value, origin),
            Err(error) => json_error(StatusCode(400), &error.to_string(), origin),
        }
    }

    fn engine_response<F>(&self, origin: Option<&str>, operation: F) -> BridgeResponse
    where
        F: FnOnce(&Engine) -> Result<serde_json::Value>,
    {
        match Engine::open_read_only(&self.project).and_then(|engine| operation(&engine)) {
            Ok(value) => json_response(StatusCode(200), &value, origin),
            Err(error) => json_error(StatusCode(500), &error.to_string(), origin),
        }
    }

    fn state_response<F>(&self, origin: Option<&str>, operation: F) -> BridgeResponse
    where
        F: FnOnce(&StateStore) -> Result<serde_json::Value>,
    {
        match StateStore::open(&self.project).and_then(|state| operation(&state)) {
            Ok(value) => json_response(StatusCode(200), &value, origin),
            Err(error) => json_error(StatusCode(500), &error.to_string(), origin),
        }
    }

    fn rotate(&self) -> Result<()> {
        let next_code = random_decimal_code()?;
        let next_token = random_token()?;
        *self
            .pairing_code
            .lock()
            .map_err(|_| anyhow::anyhow!("dashboard pairing lock is poisoned"))? = Some(next_code);
        *self
            .token
            .lock()
            .map_err(|_| anyhow::anyhow!("dashboard token lock is poisoned"))? = next_token;
        *self
            .failed_pair_attempts
            .lock()
            .map_err(|_| anyhow::anyhow!("dashboard pairing lock is poisoned"))? = 0;
        self.request_times
            .lock()
            .map_err(|_| anyhow::anyhow!("dashboard rate-limit lock is poisoned"))?
            .clear();
        let mut generation = self
            .generation
            .lock()
            .map_err(|_| anyhow::anyhow!("dashboard generation lock is poisoned"))?;
        *generation = generation.saturating_add(1);
        drop(generation);
        self.write_state()
    }

    fn write_state(&self) -> Result<()> {
        let state = DashboardStatus {
            running: true,
            pid: std::process::id(),
            address: self.address,
            project: self.project.display().to_string(),
            allowed_origins: self.allowed_origins.clone(),
            pairing_code: self
                .pairing_code
                .lock()
                .map_err(|_| anyhow::anyhow!("dashboard pairing lock is poisoned"))?
                .clone(),
            generation: *self
                .generation
                .lock()
                .map_err(|_| anyhow::anyhow!("dashboard generation lock is poisoned"))?,
            started_unix_ms: self.started_unix_ms,
        };
        atomic_file::write_atomic(&self.state_path, &serde_json::to_vec_pretty(&state)?)
            .context("write dashboard state")?;
        restrict_state_permissions(&self.state_path)
    }
}

type BridgeResponse = Response<std::io::Cursor<Vec<u8>>>;

fn read_json<T: DeserializeOwned>(request: &mut Request) -> Result<T> {
    let content_length =
        header(request, "Content-Length").and_then(|value| value.parse::<u64>().ok());
    anyhow::ensure!(
        content_length.is_none_or(|length| length <= MAX_REQUEST_BYTES),
        "request body exceeds {MAX_REQUEST_BYTES} bytes"
    );
    let mut body = Vec::new();
    request
        .as_reader()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut body)?;
    anyhow::ensure!(
        body.len() as u64 <= MAX_REQUEST_BYTES,
        "request body exceeds {MAX_REQUEST_BYTES} bytes"
    );
    serde_json::from_slice(&body).context("request body must be valid JSON")
}

fn header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

fn json_response(
    status: StatusCode,
    value: &impl Serialize,
    origin: Option<&str>,
) -> BridgeResponse {
    let body = serde_json::to_vec(value)
        .unwrap_or_else(|_| b"{\"error\":\"response serialization failed\"}".to_vec());
    let mut response = Response::from_data(body)
        .with_status_code(status)
        .with_header(header_value(
            "Content-Type",
            "application/json; charset=utf-8",
        ))
        .with_header(header_value("Cache-Control", "no-store"))
        .with_header(header_value("X-Content-Type-Options", "nosniff"))
        .with_header(header_value("Referrer-Policy", "no-referrer"));
    if let Some(origin) = origin {
        response.add_header(header_value("Access-Control-Allow-Origin", origin));
        response.add_header(header_value("Vary", "Origin"));
    }
    response
}

fn json_error(status: StatusCode, message: &str, origin: Option<&str>) -> BridgeResponse {
    json_response(status, &serde_json::json!({"error": message}), origin)
}

fn asset_response(bytes: &[u8], content_type: &str) -> BridgeResponse {
    Response::from_data(bytes.to_vec())
        .with_status_code(StatusCode(200))
        .with_header(header_value("Content-Type", content_type))
        .with_header(header_value("Content-Security-Policy", CSP))
        .with_header(header_value("Cache-Control", "no-store"))
        .with_header(header_value("X-Content-Type-Options", "nosniff"))
        .with_header(header_value("X-Frame-Options", "DENY"))
        .with_header(header_value("Referrer-Policy", "no-referrer"))
}

fn empty_response(
    status: StatusCode,
    origin: Option<&str>,
    private_network: bool,
) -> BridgeResponse {
    let mut response = json_response(status, &serde_json::json!({}), origin);
    response.add_header(header_value(
        "Access-Control-Allow-Headers",
        "Authorization, Content-Type",
    ));
    response.add_header(header_value(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS",
    ));
    response.add_header(header_value("Access-Control-Max-Age", "600"));
    if private_network {
        response.add_header(header_value("Access-Control-Allow-Private-Network", "true"));
    }
    response
}

fn header_value(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header is valid")
}

fn validate_origins(origins: &[String]) -> Result<Vec<String>> {
    anyhow::ensure!(
        origins.len() <= MAX_ALLOWED_ORIGINS,
        "at most {MAX_ALLOWED_ORIGINS} dashboard origins are allowed"
    );
    let mut validated = Vec::new();
    for origin in origins {
        anyhow::ensure!(
            !origin.is_empty() && origin.len() <= MAX_ORIGIN_BYTES,
            "dashboard origin has an invalid length"
        );
        let valid = origin
            .strip_prefix("https://")
            .is_some_and(valid_origin_authority)
            || origin.strip_prefix("http://").is_some_and(|authority| {
                (authority.starts_with("127.0.0.1:") || authority.starts_with("localhost:"))
                    && valid_origin_authority(authority)
            });
        anyhow::ensure!(
            valid,
            "dashboard origin must be an HTTPS origin or loopback HTTP origin"
        );
        if !validated.contains(origin) {
            validated.push(origin.clone());
        }
    }
    Ok(validated)
}

fn valid_origin_authority(authority: &str) -> bool {
    !authority.is_empty()
        && !authority.contains(['/', '?', '#', '@'])
        && !authority.chars().any(char::is_whitespace)
}

fn random_decimal_code() -> Result<String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).context("generate dashboard pairing code")?;
    let value = u64::from_le_bytes(bytes) % 100_000_000;
    Ok(format!("{value:08}"))
}

fn random_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).context("generate dashboard bearer token")?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left = left.get(index).copied().unwrap_or(0);
        let right = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

fn validate_project_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty() && name.len() <= 63,
        "dashboard project name must contain 1-63 characters"
    );
    anyhow::ensure!(
        name.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-'),
        "dashboard project name may contain lowercase letters, digits, and interior hyphens"
    );
    Ok(())
}

fn require_command(command: &str, arguments: &[&str]) -> Result<()> {
    let status = Command::new(command)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "{command} is required; install it and authenticate before deploying the dashboard"
            )
        })?;
    anyhow::ensure!(
        status.success(),
        "{command} is installed but not usable; authenticate it and retry"
    );
    Ok(())
}

fn temporary_deploy_directory() -> Result<PathBuf> {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).context("allocate dashboard deployment directory")?;
    let directory = std::env::temp_dir().join(format!(
        "structurely-dashboard-{}-{}",
        std::process::id(),
        hex(&random)
    ));
    fs::create_dir(&directory)
        .with_context(|| format!("create deployment directory {}", directory.display()))?;
    Ok(directory)
}

fn deployment_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|character: char| {
                    matches!(character, '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';')
                })
                .to_owned()
        })
        .rfind(|token| token.starts_with("https://"))
}

fn bridge_health(address: SocketAddr) -> bool {
    if !address.ip().is_loopback() {
        return false;
    }
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(250)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    if write!(
        stream,
        "GET /api/v1/health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .is_err()
    {
        return false;
    }
    let mut response = [0_u8; 64];
    stream
        .read(&mut response)
        .is_ok_and(|read| response[..read].starts_with(b"HTTP/1.1 200"))
}

fn remove_if_present(path: &std::path::Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(unix)]
fn restrict_state_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("restrict dashboard state permissions")
}

#[cfg(not(unix))]
fn restrict_state_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_fixed_width_hex() {
        let first = random_token().unwrap();
        let second = random_token().unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn comparison_handles_equal_different_and_mismatched_lengths() {
        assert!(constant_time_eq(b"pairing", b"pairing"));
        assert!(!constant_time_eq(b"pairing", b"Pairing"));
        assert!(!constant_time_eq(b"pairing", b"pairing-extra"));
    }

    #[test]
    fn origin_validation_rejects_remote_http_and_paths() {
        assert!(validate_origins(&["https://dashboard.example".to_owned()]).is_ok());
        assert!(validate_origins(&["http://dashboard.example".to_owned()]).is_err());
        assert!(validate_origins(&["https://dashboard.example/path".to_owned()]).is_err());
    }

    #[test]
    fn project_names_and_provider_urls_are_conservative() {
        assert!(validate_project_name("structurely-dashboard").is_ok());
        assert!(validate_project_name("../dashboard").is_err());
        assert!(validate_project_name("Dashboard").is_err());
        assert_eq!(
            deployment_url("Uploading\nProduction: https://structurely.vercel.app\n"),
            Some("https://structurely.vercel.app".to_owned())
        );
    }
}
