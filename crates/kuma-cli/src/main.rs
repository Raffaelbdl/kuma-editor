use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use kuma_daemon::{Config, find_repo_root};

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("KUMA_GIT"), ")");

#[derive(Parser)]
#[command(name = "kuma", version = VERSION, about = "Hybrid video editor: MLT timeline + HTML overlays + MCP")]
struct Cli {
    /// Project file (`*.kuma.json`). Created if missing.
    #[arg(short, long, global = true, env = "KUMA_PROJECT", default_value = "project.kuma.json")]
    project: PathBuf,
    /// Repository root holding `overlays/` and `node_modules/hyperframes`.
    #[arg(long, global = true, env = "KUMA_REPO")]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon (HTTP API, UI, preview) in the foreground.
    Serve {
        #[arg(long, default_value = "127.0.0.1:7455")]
        bind: SocketAddr,
    },
    /// Run as an MCP server on stdio (starts the daemon in-process unless one is running).
    Mcp {
        #[arg(long, default_value = "127.0.0.1:7455")]
        bind: SocketAddr,
        /// Use an already-running daemon at this URL instead of starting one.
        #[arg(long, env = "KUMA_DAEMON_URL")]
        daemon: Option<String>,
    },
    /// Render the project with an export preset.
    Render {
        #[arg(long, default_value = "youtube-1080p")]
        preset: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Supersample overlays (2 = render at 2× DPR and downscale).
        #[arg(long, default_value_t = 1)]
        supersample: u32,
    },
    /// Bake all overlay clips (or the given clip ids).
    Bake {
        clips: Vec<String>,
        #[arg(long, default_value_t = 1)]
        supersample: u32,
    },
    /// Sample composited frames as PNGs.
    Sample {
        t0: f64,
        t1: Option<f64>,
        #[arg(short, long, default_value_t = 1)]
        count: usize,
        /// Crop rect `x,y,w,h` in composition pixels.
        #[arg(long)]
        rect: Option<String>,
        #[arg(long)]
        width: Option<u32>,
    },
    /// Write the compiled MLT XML to stdout (overlays skipped unless baked).
    Compile,
    /// Validate the project and print issues.
    Validate,
    /// Print the JSON Schema for project files.
    Schema,
    /// Check the installation: repo, tools, engine, daemon, MCP config.
    Doctor,
    /// Print the export presets.
    Presets,
    /// List overlay templates.
    Templates,
    /// Generate `thumb.png` for overlay templates (all, or the given ids).
    Thumbs { templates: Vec<String> },
    /// Register a media file as an asset (probes it with ffprobe).
    AddAsset {
        path: PathBuf,
        #[arg(long)]
        id: Option<String>,
    },
    /// Create a new empty project file (and a project-scoped `.mcp.json` for Claude Code).
    Init {
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value = "30")]
        fps: String,
        /// Do not write `.mcp.json` next to the project.
        #[arg(long)]
        no_mcp: bool,
    },
    /// Write (or print) a project-scoped `.mcp.json` so `claude` in this directory gets the kuma server.
    McpConfig {
        /// Print the JSON instead of writing the file.
        #[arg(long)]
        print: bool,
    },
}

/// `path` as the user meant it: relative to the shell's cwd, normalized when it exists.
fn absolute(path: &std::path::Path) -> anyhow::Result<PathBuf> {
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    Ok(abs.canonicalize().unwrap_or(abs))
}

fn repo_root(cli: &Cli) -> anyhow::Result<PathBuf> {
    cli.repo.clone().or_else(find_repo_root).ok_or_else(|| anyhow::anyhow!("cannot find the kuma repository root (overlays/); pass --repo or set KUMA_REPO"))
}

fn config(cli: &Cli, bind: SocketAddr) -> anyhow::Result<Config> {
    let repo_root = repo_root(cli)?;
    let ui_dir = repo_root.join("ui/dist");
    let engine = repo_root.join("engine/build/kuma-preview");
    Ok(Config {
        project_file: absolute(&cli.project)?,
        repo_root,
        ui_dir: ui_dir.is_dir().then_some(ui_dir),
        bind,
        preview_engine: engine.is_file().then_some(engine),
    })
}

/// Load the project state for one-shot commands (no server).
fn open(cli: &Cli) -> anyhow::Result<std::sync::Arc<kuma_daemon::AppState>> {
    Ok(std::sync::Arc::new(kuma_daemon::AppState::load(config(cli, "127.0.0.1:0".parse().unwrap())?)?))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let log_to_stderr = matches!(cli.cmd, Cmd::Mcp { .. });
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,hyper=warn,tower_http=warn".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    if log_to_stderr {
        builder.with_writer(std::io::stderr).init();
    } else {
        builder.init();
    }

    match &cli.cmd {
        Cmd::Serve { bind } => {
            let cfg = config(&cli, *bind)?;
            let (state, addr) = kuma_daemon::serve(cfg).await?;
            println!("kuma: http://{addr}  project={}", state.config.project_file.display());
            tokio::signal::ctrl_c().await?;
        }
        Cmd::Mcp { bind, daemon } => {
            let (url, note) = match daemon {
                Some(u) => (u.clone(), None),
                None => match resolve_daemon(&cli, *bind).await {
                    Ok(r) => r,
                    Err(e) => {
                        // Never die silently: Claude Code only shows "Connection closed".
                        tracing::error!("kuma mcp cannot start: {e:#}");
                        eprintln!("kuma mcp: {e:#}\nRun `kuma doctor --project <file>` for details.");
                        std::process::exit(2);
                    }
                },
            };
            tracing::info!("kuma mcp serving stdio, daemon at {url}");
            kuma_mcp::run_stdio(url, note).await?;
        }
        Cmd::Doctor => {
            let cfg = config(&cli, "127.0.0.1:7455".parse().unwrap())?;
            println!("binary        {} — version {VERSION}", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default());
            println!("repo root     {}", cfg.repo_root.display());
            println!("project       {} ({})", cfg.project_file.display(), if cfg.project_file.is_file() { "exists" } else { "will be created" });
            println!("overlays      {} templates", kuma_core::template::TemplateManifest::load_all(&cfg.repo_root.join("overlays")).len());
            println!("ui            {}", cfg.ui_dir.as_ref().map(|p| p.display().to_string()).unwrap_or("ui/dist missing — run `cd ui && npm run build`".into()));
            let engine = cfg.repo_root.join("engine/build/kuma-preview");
            println!("engine        {}", if engine.is_file() { engine.display().to_string() } else { "missing — cmake -S engine -B engine/build && cmake --build engine/build -j".into() });
            println!("melt          {}", if kuma_mlt::melt::melt_available() { "ok" } else { "MISSING (apt install melt)" });
            println!("ffprobe       {}", if kuma_overlay::media::ffprobe_available() { "ok" } else { "MISSING (apt install ffmpeg)" });
            let baker = kuma_overlay::Baker::discover(&cfg.repo_root, cfg.cache_dir().join("bakes"));
            println!("hyperframes   {}", baker.available().map(|v| format!("{v} ({})", baker.hyperframes.display())).unwrap_or_else(|e| format!("MISSING — npm install at the repo root ({e})")));
            println!("browser       {}", baker.browser.as_ref().map(|p| p.display().to_string()).unwrap_or("not set — hyperframes will download Chrome headless shell on first bake".into()));
            println!("display       {}", if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() { "yes" } else if kuma_daemon::preview::has_bin("xvfb-run") { "none; xvfb-run available" } else { "NONE and no xvfb-run — melt/engine will fail (apt install xvfb)" });
            let url = format!("http://{}", cfg.bind);
            match kuma_mcp::client::health(&url).await {
                Some(h) => println!("daemon        running at {url} for {}", h["project"].as_str().unwrap_or("?")),
                None if port_busy(cfg.bind) => println!("daemon        port {} is busy but not answering /api/health", cfg.bind),
                None => println!("daemon        not running (kuma serve, or kuma mcp starts one)"),
            }
            let mcp = cfg.project_root().join(".mcp.json");
            println!("mcp config    {}", if mcp.is_file() { mcp.display().to_string() } else { "none — run `kuma mcp-config`".into() });
        }
        Cmd::Render { preset, output, supersample } => {
            let state = open(&cli)?;
            let output = output.as_deref().map(absolute).transpose()?;
            let req = kuma_daemon::jobs::RenderRequest { preset: preset.clone(), output, range: None, supersample: *supersample };
            let st = state.clone();
            let out = tokio::task::spawn_blocking(move || kuma_daemon::jobs::render(&st, &req, &|p, m| eprintln!("[{p:3}%] {m}"))).await??;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::Bake { clips, supersample } => {
            let state = open(&cli)?;
            let project = state.project.read().unwrap().clone();
            let only = if clips.is_empty() { None } else { Some(clips.clone()) };
            let ss = *supersample;
            let st = state.clone();
            let bakes = tokio::task::spawn_blocking(move || {
                kuma_daemon::jobs::bake_all(&st, &project, only.as_deref(), kuma_overlay::Format::PngSequence, ss, &|p, m| eprintln!("[{p:3}%] {m}"))
            })
            .await??;
            for (k, b) in bakes {
                println!("{k}\t{}", b.resource.display());
            }
        }
        Cmd::Sample { t0, t1, count, rect, width } => {
            let state = open(&cli)?;
            let rect = match rect {
                Some(s) => {
                    let v: Vec<u32> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
                    anyhow::ensure!(v.len() == 4, "rect must be x,y,w,h");
                    Some(kuma_core::template::Region { x: v[0], y: v[1], w: v[2], h: v[3] })
                }
                None => None,
            };
            let req = kuma_daemon::jobs::SampleRequest { t0: *t0, t1: *t1, count: *count, rect, width: *width, overlays: true };
            let st = state.clone();
            let samples = tokio::task::spawn_blocking(move || kuma_daemon::jobs::sample_frames(&st, &req, &|_, _| {})).await??;
            println!("{}", serde_json::to_string_pretty(&samples)?);
        }
        Cmd::Compile => {
            let state = open(&cli)?;
            let project = state.project.read().unwrap().clone();
            let path = kuma_daemon::jobs::compile_project(&state, &project, Default::default(), "inspect", kuma_mlt::UnbakedPolicy::Skip, None)?;
            print!("{}", std::fs::read_to_string(path)?);
        }
        Cmd::Validate => {
            let state = open(&cli)?;
            let project = state.project.read().unwrap().clone();
            let issues = kuma_core::validate(&project, &state.validation_context());
            println!("{}", serde_json::to_string_pretty(&issues)?);
            if kuma_core::validate::has_errors(&issues) {
                std::process::exit(1);
            }
        }
        Cmd::Schema => println!("{}", serde_json::to_string_pretty(&kuma_core::schema::project_schema())?),
        Cmd::Presets => {
            for p in kuma_mlt::PRESETS {
                println!("{:16} .{:5} {}", p.id, p.extension, p.description);
            }
        }
        Cmd::Templates => {
            let state = open(&cli)?;
            for t in state.template_infos() {
                println!("{:16} {:24} vars={}", t.id, t.manifest.name, t.manifest.vars.keys().cloned().collect::<Vec<_>>().join(","));
            }
        }
        Cmd::Thumbs { templates } => {
            let state = open(&cli)?;
            let project = state.project.read().unwrap().clone();
            let all = state.templates.read().unwrap().clone();
            for (id, (dir, manifest)) in all.iter().filter(|(id, _)| templates.is_empty() || templates.contains(id)) {
                let started = std::time::Instant::now();
                let out = state.baker.thumbnail(dir, manifest, project.width, project.height).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
                println!("{id:16} {} ({:.1}s)", out.display(), started.elapsed().as_secs_f64());
            }
        }
        Cmd::AddAsset { path, id } => {
            // The daemon owns the project while it runs: route the edit through it rather
            // than writing the file underneath it.
            let path = absolute(path)?;
            let cfg = config(&cli, "127.0.0.1:7455".parse().unwrap())?;
            let url = format!("http://{}", cfg.bind);
            let served = kuma_mcp::client::health(&url).await.and_then(|h| h["project"].as_str().map(std::path::PathBuf::from)).and_then(|p| p.canonicalize().ok());
            let (id, rev) = if served.as_ref() == Some(&cfg.project_file) {
                let body = serde_json::json!({ "path": path, "id": id, "origin": "cli: add asset" });
                let v = kuma_mcp::client::Client::new(&url).post("/api/assets", &body).await?;
                (v["id"].as_str().unwrap_or_default().to_string(), v["revision"].as_u64().unwrap_or_default())
            } else {
                let state = open(&cli)?;
                let (id, rev, _) = kuma_daemon::jobs::add_asset(&state, &path, id.clone(), None, "cli")?;
                (id, rev)
            };
            println!("added asset `{id}` (revision {rev})");
        }
        Cmd::McpConfig { print } => {
            let path = absolute(&cli.project)?;
            let json = mcp_config_json(&path, &repo_root(&cli)?)?;
            if *print {
                println!("{json}");
            } else {
                let out = path.parent().unwrap().join(".mcp.json");
                write_mcp_config(&out, &json)?;
                println!("wrote {}", out.display());
            }
        }
        Cmd::Init { width, height, fps, no_mcp } => {
            let path = absolute(&cli.project)?;
            anyhow::ensure!(!path.exists(), "{} already exists", path.display());
            // Accepts `30`, `29.97`, `30000/1001` or `{"num":30000,"den":1001}`.
            let fps = if let Some((n, d)) = fps.split_once('/') {
                kuma_core::Fps::new(n.trim().parse()?, d.trim().parse()?)
            } else {
                serde_json::from_str::<kuma_core::Fps>(fps)?
            };
            let p = kuma_core::Project::new_default(kuma_core::Project::name_from_path(&path), *width, *height, fps);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, serde_json::to_string_pretty(&p)?)?;
            println!("created {}", path.display());
            if !*no_mcp {
                match repo_root(&cli) {
                    Ok(repo) => {
                        let path = absolute(&path)?;
                        let out = path.parent().unwrap().join(".mcp.json");
                        write_mcp_config(&out, &mcp_config_json(&path, &repo)?)?;
                        println!("wrote {} — run `claude` in this directory to use the kuma MCP server", out.display());
                    }
                    Err(e) => eprintln!("note: {e}; skipping .mcp.json (run `kuma mcp-config` later)"),
                }
            }
        }
    }
    Ok(())
}

/// A project-scoped Claude Code MCP entry. Absolute paths, so it works even when `kuma`
/// is not on PATH and regardless of where `claude` resolves relative paths.
fn mcp_config_json(project_file: &std::path::Path, repo_root: &std::path::Path) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());
    let v = serde_json::json!({
        "mcpServers": {
            "kuma": {
                "command": exe,
                "args": ["mcp", "--project", project_file],
                "env": { "KUMA_REPO": repo }
            }
        }
    });
    Ok(serde_json::to_string_pretty(&v)?)
}

/// Write `.mcp.json`, merging into an existing file's `mcpServers` if there is one. A file
/// we cannot parse is left alone rather than overwritten.
fn write_mcp_config(out: &std::path::Path, json: &str) -> anyhow::Result<()> {
    let new: serde_json::Value = serde_json::from_str(json)?;
    let merged = match std::fs::read_to_string(out) {
        Ok(text) => {
            let bad = |what: &str| anyhow::anyhow!("{}: {what}; fix or delete it and rerun", out.display());
            let mut existing: serde_json::Value = serde_json::from_str(&text).map_err(|e| bad(&format!("not valid JSON ({e})")))?;
            let servers = existing.as_object_mut().ok_or_else(|| bad("top level is not an object"))?.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
            servers.as_object_mut().ok_or_else(|| bad("`mcpServers` is not an object"))?.insert("kuma".into(), new["mcpServers"]["kuma"].clone());
            existing
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => new,
        Err(e) => anyhow::bail!("cannot read {}: {e}", out.display()),
    };
    std::fs::write(out, serde_json::to_string_pretty(&merged)?)?;
    Ok(())
}

/// Decide which daemon the MCP server talks to:
/// 1. a running daemon on the port that serves *this* project → use it;
/// 2. a running daemon serving another project, or a busy port → start our own on a free port;
/// 3. nothing there → start our own on the port.
async fn resolve_daemon(cli: &Cli, bind: SocketAddr) -> anyhow::Result<(String, Option<String>)> {
    let mut cfg = config(cli, bind)?;
    let url = format!("http://{bind}");
    let wanted = cfg.project_file.canonicalize().unwrap_or(cfg.project_file.clone());
    let note;
    match kuma_mcp::client::health(&url).await {
        Some(h) => {
            let theirs = h["project"].as_str().map(std::path::PathBuf::from).and_then(|p| p.canonicalize().ok());
            if theirs.as_ref() == Some(&wanted) {
                tracing::info!("using running daemon at {url}");
                return Ok((url, None));
            }
            let theirs_s = theirs.as_ref().map(|p| p.display().to_string()).unwrap_or("?".into());
            tracing::warn!("daemon at {url} serves {theirs_s}, not {}; starting a private one", wanted.display());
            note = format!(
                "This MCP server is connected to a PRIVATE daemon for {} because the daemon at {url} (the one behind the browser UI) serves a different project, {theirs_s}. Edits here will not show in that UI. To unify: run `kuma mcp-config --project {theirs_s}` and reconnect, or restart `kuma serve -p {}`.",
                wanted.display(), wanted.display()
            );
        }
        None if port_busy(bind) => {
            tracing::warn!("port {bind} is busy but not a kuma daemon; starting a private one");
            note = format!("Port {bind} is busy but not a kuma daemon; this MCP server started a private daemon for {}.", wanted.display());
        }
        None => match kuma_daemon::serve(cfg.clone()).await {
            Ok((_state, addr)) => {
                tracing::info!("started daemon at http://{addr}");
                return Ok((format!("http://{addr}"), None));
            }
            Err(e) => {
                // Two `kuma mcp` starting at once (two Claude Code windows): the loser
                // joins the winner once it answers, which can take a moment after the bind.
                for _ in 0..10 {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    if let Some(h) = kuma_mcp::client::health(&url).await {
                        if h["project"].as_str().map(std::path::PathBuf::from).and_then(|p| p.canonicalize().ok()).as_ref() == Some(&wanted) {
                            tracing::info!("using daemon that came up meanwhile at {url}");
                            return Ok((url, None));
                        }
                        break;
                    }
                }
                tracing::warn!("cannot bind {bind} ({e:#}); starting a private daemon");
                note = format!("Port {bind} could not be bound; this MCP server started a private daemon for {}.", wanted.display());
            }
        },
    }
    cfg.bind = "127.0.0.1:0".parse().unwrap();
    let (_state, addr) = kuma_daemon::serve(cfg).await.map_err(|e| anyhow::anyhow!("starting private daemon: {e:#}"))?;
    tracing::info!("started private daemon at http://{addr} (UI for this project: open that URL)");
    Ok((format!("http://{addr}"), Some(format!("{note} Its own UI is at http://{addr}."))))
}

fn port_busy(addr: SocketAddr) -> bool {
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)).is_ok()
}
