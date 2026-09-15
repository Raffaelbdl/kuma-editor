use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use kuma_daemon::{default_library_dirs, find_repo_root, Config};

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("KUMA_GIT"), ")");
const DEFAULT_BIND: &str = "127.0.0.1:7455";

#[derive(Parser)]
#[command(name = "kuma", version = VERSION, about = "Hybrid video editor: MLT timeline + HTML overlays + MCP")]
struct Cli {
    /// Project file (`*.kuma.json`). Created if missing.
    #[arg(short, long, global = true, env = "KUMA_PROJECT", default_value = "project.kuma.json")]
    project: PathBuf,
    /// Repository root holding `node_modules/hyperframes`, `ui/dist` and `engine/build`.
    #[arg(long, global = true, env = "KUMA_REPO")]
    repo: Option<PathBuf>,
    /// Address the daemon listens on (and where other commands look for it).
    #[arg(long, global = true, env = "KUMA_BIND", default_value = DEFAULT_BIND)]
    bind: SocketAddr,
    /// Sequence (bin id) to render, sample or compile. The project's master when unset.
    #[arg(long, global = true)]
    sequence: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon (HTTP API, UI, preview) in the foreground.
    Serve,
    /// Run as an MCP server on stdio (starts the daemon in-process unless one is running).
    Mcp {
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
    },
    /// Bake all overlay clips (or the given clip ids).
    Bake { clips: Vec<String> },
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
    /// Validate the project and print issues (exit 1 on errors).
    Validate,
    /// Check the installation: repo, tools, engine, library, daemon, MCP config.
    Doctor,
    /// Generate `thumb.png` for overlay templates (all, or the given ids).
    Thumbs { templates: Vec<String> },
    /// Install the starter templates and presets into the user library (`~/.local/share/kuma`).
    Library {
        /// Overwrite templates that already exist in the library.
        #[arg(long)]
        force: bool,
    },
    /// Probe a media file and add it to the project bin.
    AddMedia {
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
fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    Ok(abs.canonicalize().unwrap_or(abs))
}

fn repo_root(cli: &Cli) -> anyhow::Result<PathBuf> {
    cli.repo.clone().or_else(find_repo_root).ok_or_else(|| anyhow::anyhow!("cannot find the kuma repository root (node_modules/hyperframes); pass --repo or set KUMA_REPO"))
}

fn config(cli: &Cli) -> anyhow::Result<Config> {
    Ok(Config { project_file: absolute(&cli.project)?, repo_root: repo_root(cli)?, library_dirs: default_library_dirs(), bind: cli.bind })
}

/// Load the project state for one-shot commands (no server).
fn open(cli: &Cli) -> anyhow::Result<std::sync::Arc<kuma_daemon::AppState>> {
    Ok(std::sync::Arc::new(kuma_daemon::AppState::load(config(cli)?)?))
}

/// The running daemon for *this* project at `--bind`, if any.
async fn running_daemon(cli: &Cli) -> Option<String> {
    let url = format!("http://{}", cli.bind);
    let h = kuma_mcp::client::health(&url).await?;
    let theirs = h["project"].as_str().map(PathBuf::from).and_then(|p| p.canonicalize().ok())?;
    (theirs == absolute(&cli.project).ok()?).then_some(url)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,hyper=warn,tower_http=warn".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    if matches!(cli.cmd, Cmd::Mcp { .. }) {
        builder.with_writer(std::io::stderr).init();
    } else {
        builder.init();
    }

    let sequence = cli.sequence.clone();
    match &cli.cmd {
        Cmd::Serve => {
            let (state, addr) = kuma_daemon::serve(config(&cli)?).await?;
            println!("kuma: http://{addr}  project={}", state.config.project_file.display());
            tokio::signal::ctrl_c().await?;
        }
        Cmd::Mcp { daemon } => {
            let (url, note) = match daemon {
                Some(u) => (u.clone(), None),
                None => match resolve_daemon(&cli).await {
                    Ok(r) => r,
                    Err(e) => {
                        // Never die silently: Claude Code only shows "Connection closed".
                        eprintln!("kuma mcp: {e:#}\nRun `kuma doctor --project <file>` for details.");
                        std::process::exit(2);
                    }
                },
            };
            tracing::info!("kuma mcp serving stdio, daemon at {url}");
            kuma_mcp::run_stdio(url, note).await?;
        }
        Cmd::Doctor => doctor(&cli).await?,
        Cmd::Render { preset, output } => {
            let state = open(&cli)?;
            let req = kuma_daemon::jobs::RenderRequest { preset: preset.clone(), output: output.as_deref().map(absolute).transpose()?, range: None, sequence: sequence.clone() };
            let out = tokio::task::spawn_blocking(move || kuma_daemon::jobs::render(&state, &req, &|p, m| eprintln!("[{p:3}%] {m}"))).await??;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::Bake { clips } => {
            let state = open(&cli)?;
            let only = (!clips.is_empty()).then(|| clips.clone());
            let bakes = tokio::task::spawn_blocking(move || {
                let project = state.project.read().unwrap().clone();
                kuma_daemon::jobs::bake_all(&state, &project, only.as_deref(), &|p, m| eprintln!("[{p:3}%] {m}"))
            })
            .await??;
            for (k, b) in bakes {
                println!("{k}\t{}", b.resource.display());
            }
        }
        Cmd::Sample { t0, t1, count, rect, width } => {
            let state = open(&cli)?;
            let rect = rect
                .as_deref()
                .map(|s| {
                    let v: Vec<u32> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
                    anyhow::ensure!(v.len() == 4, "rect must be x,y,w,h");
                    Ok::<_, anyhow::Error>(kuma_core::template::Region { x: v[0], y: v[1], w: v[2], h: v[3] })
                })
                .transpose()?;
            let req = kuma_daemon::jobs::SampleRequest { t0: *t0, t1: *t1, count: *count, rect, width: *width, overlays: true, sequence: sequence.clone(), nested_overlays_only: false };
            let samples = tokio::task::spawn_blocking(move || kuma_daemon::jobs::sample_frames(&state, &req, &|_, _| {})).await??;
            println!("{}", serde_json::to_string_pretty(&samples)?);
        }
        Cmd::Compile => {
            let state = open(&cli)?;
            let project = state.project.read().unwrap().clone();
            print!("{}", kuma_daemon::jobs::compile_xml(&state, &project, Default::default(), kuma_mlt::UnbakedPolicy::Skip, None, sequence.clone())?);
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
        Cmd::Thumbs { templates } => {
            let state = open(&cli)?;
            let all = state.templates.read().unwrap().loaded.clone();
            let empty = serde_json::Map::new();
            for (id, (dir, manifest)) in all.iter().filter(|(id, _)| templates.is_empty() || templates.contains(id)) {
                let out = dir.join("thumb.png");
                state.baker.thumbnail(dir, manifest, &empty, &out).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
                println!("{id:16} {}", out.display());
            }
        }
        Cmd::Library { force } => {
            let src = repo_root(&cli)?.join("library");
            let dst = default_library_dirs().remove(0);
            let n = install_library(&src, &dst, *force)?;
            println!("installed {n} entries into {}", dst.display());
        }
        Cmd::AddMedia { path, id } => {
            // The daemon owns the project while it runs: route the edit through it rather
            // than writing the file underneath it.
            let path = absolute(path)?;
            let (id, rev) = if let Some(url) = running_daemon(&cli).await {
                let body = serde_json::json!({ "path": path, "id": id, "origin": "cli: add media" });
                let v = kuma_mcp::client::Client::new(&url).post("/api/bin", &body).await?;
                (v["id"].as_str().unwrap_or_default().to_string(), v["revision"].as_u64().unwrap_or_default())
            } else {
                let state = open(&cli)?;
                let (id, rev, _) = kuma_daemon::jobs::add_media(&state, &path, id.clone(), None, "cli")?;
                (id, rev)
            };
            println!("added bin entry `{id}` (revision {rev})");
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
            let fps = match fps.split_once('/') {
                Some((n, d)) => kuma_core::Fps::new(n.trim().parse()?, d.trim().parse()?),
                None => serde_json::from_str::<kuma_core::Fps>(fps)?,
            };
            let p = kuma_core::Project::new_default(kuma_core::Project::name_from_path(&path), *width, *height, fps);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, serde_json::to_string_pretty(&p)?)?;
            println!("created {}", path.display());
            let library = default_library_dirs().remove(0);
            if !library.join("overlays").is_dir() {
                println!("note: no template library at {} — run `kuma library` to install the starter set", library.display());
            }
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

async fn doctor(cli: &Cli) -> anyhow::Result<()> {
    let cfg = config(cli)?;
    let ok = |b: bool, yes: &str, no: &str| if b { yes.to_string() } else { format!("MISSING — {no}") };
    println!("binary        {} — version {VERSION}", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default());
    println!("repo root     {}", cfg.repo_root.display());
    println!("project       {} ({})", cfg.project_file.display(), if cfg.project_file.is_file() { "exists" } else { "will be created" });
    for d in cfg.all_template_dirs() {
        let (loaded, errors) = kuma_core::template::TemplateManifest::load_all(&d);
        println!("templates     {} — {} loaded{}", d.display(), loaded.len(), if errors.is_empty() { String::new() } else { format!(", {} failed: {}", errors.len(), errors[0]) });
    }
    println!("ui            {}", ok(cfg.repo_root.join("ui/dist").is_dir(), "ui/dist", "cd ui && npm run build"));
    println!("engine        {}", ok(cfg.repo_root.join("engine/build/kuma-preview").is_file(), "engine/build/kuma-preview", "cmake -S engine -B engine/build && cmake --build engine/build -j"));
    println!("melt          {}", ok(kuma_mlt::melt::melt_available(), "ok", "apt install melt"));
    println!("ffprobe       {}", ok(kuma_overlay::media::ffprobe_available(), "ok", "apt install ffmpeg"));
    let baker = kuma_overlay::Baker::discover(&cfg.repo_root, PathBuf::new(), cfg.cache_dir().join("bakes"));
    println!("hyperframes   {}", baker.available().map(|v| format!("{v} ({})", baker.hyperframes.display())).unwrap_or_else(|e| format!("MISSING — npm install at the repo root ({e})")));
    println!("browser       {}", baker.browser.as_ref().map(|p| p.display().to_string()).unwrap_or("not set — hyperframes will download Chrome headless shell on first bake".into()));
    let display = std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    println!("display       {}", if display { "yes" } else if kuma_daemon::preview::has_bin("xvfb-run") { "none; xvfb-run available" } else { "NONE and no xvfb-run — melt/engine will fail (apt install xvfb)" });
    let url = format!("http://{}", cfg.bind);
    match kuma_mcp::client::health(&url).await {
        Some(h) => println!("daemon        running at {url} for {}", h["project"].as_str().unwrap_or("?")),
        None if port_busy(cfg.bind) => println!("daemon        port {} is busy but not answering /api/health", cfg.bind),
        None => println!("daemon        not running (kuma serve, or kuma mcp starts one)"),
    }
    let mcp = cfg.project_root().join(".mcp.json");
    println!("mcp config    {}", if mcp.is_file() { mcp.display().to_string() } else { "none — run `kuma mcp-config`".into() });
    Ok(())
}

/// Copy `<src>/{overlays,presets}` into the library, skipping entries that already exist
/// unless `force`. Returns the number of entries written.
fn install_library(src: &Path, dst: &Path, force: bool) -> anyhow::Result<usize> {
    fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            let target = dst.join(e.file_name());
            if e.path().is_dir() {
                copy_dir(&e.path(), &target)?;
            } else {
                std::fs::copy(e.path(), target)?;
            }
        }
        Ok(())
    }
    let mut n = 0;
    for kind in ["overlays", "presets"] {
        let Ok(rd) = std::fs::read_dir(src.join(kind)) else { continue };
        for e in rd.flatten() {
            let target = dst.join(kind).join(e.file_name());
            if target.exists() && !force {
                continue;
            }
            if e.path().is_dir() {
                copy_dir(&e.path(), &target)?;
            } else {
                std::fs::create_dir_all(target.parent().unwrap())?;
                std::fs::copy(e.path(), &target)?;
            }
            n += 1;
        }
    }
    Ok(n)
}

/// A project-scoped Claude Code MCP entry. Absolute paths, so it works even when `kuma`
/// is not on PATH and regardless of where `claude` resolves relative paths.
fn mcp_config_json(project_file: &Path, repo_root: &Path) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());
    let v = serde_json::json!({
        "mcpServers": { "kuma": { "command": exe, "args": ["mcp", "--project", project_file], "env": { "KUMA_REPO": repo } } }
    });
    Ok(serde_json::to_string_pretty(&v)?)
}

/// Write `.mcp.json`, merging into an existing file's `mcpServers` if there is one. A file
/// we cannot read or parse is left alone rather than overwritten.
fn write_mcp_config(out: &Path, json: &str) -> anyhow::Result<()> {
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
/// 1. a running daemon on `--bind` that serves *this* project → use it;
/// 2. a daemon serving another project, a busy port, or a lost bind race → a private
///    daemon on a free port;
/// 3. nothing there → start our own on `--bind`.
async fn resolve_daemon(cli: &Cli) -> anyhow::Result<(String, Option<String>)> {
    let mut cfg = config(cli)?;
    let url = format!("http://{}", cfg.bind);
    let wanted = cfg.project_file.clone();
    let serves_us = |h: &serde_json::Value| h["project"].as_str().map(PathBuf::from).and_then(|p| p.canonicalize().ok()).as_ref() == Some(&wanted);
    let note = match kuma_mcp::client::health(&url).await {
        Some(h) if serves_us(&h) => return Ok((url, None)),
        Some(h) => {
            let theirs = h["project"].as_str().unwrap_or("?");
            format!("This MCP server is connected to a PRIVATE daemon for {} because the daemon at {url} (the one behind the browser UI) serves a different project, {theirs}. Edits here will not show in that UI. To unify: run `kuma mcp-config --project {theirs}` and reconnect, or restart `kuma serve -p {}`.", wanted.display(), wanted.display())
        }
        None if port_busy(cfg.bind) => format!("Port {} is busy but not a kuma daemon; this MCP server started a private daemon for {}.", cfg.bind, wanted.display()),
        None => {
            // The project file is loaded before the bind: report its errors as such.
            match kuma_daemon::serve(cfg.clone()).await {
                Ok((_state, addr)) => return Ok((format!("http://{addr}"), None)),
                Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::AddrInUse) => {
                    // Two `kuma mcp` starting at once (two Claude Code windows): join the
                    // winner once it answers, which can take a moment after the bind.
                    for _ in 0..10 {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        if let Some(h) = kuma_mcp::client::health(&url).await {
                            if serves_us(&h) {
                                return Ok((url, None));
                            }
                            break;
                        }
                    }
                    format!("Port {} could not be bound; this MCP server started a private daemon for {}.", cfg.bind, wanted.display())
                }
                Err(e) => return Err(e),
            }
        }
    };
    tracing::warn!("{note}");
    cfg.bind = "127.0.0.1:0".parse().unwrap();
    let (_state, addr) = kuma_daemon::serve(cfg).await.map_err(|e| anyhow::anyhow!("starting private daemon: {e:#}"))?;
    Ok((format!("http://{addr}"), Some(format!("{note} Its own UI is at http://{addr}."))))
}

fn port_busy(addr: SocketAddr) -> bool {
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)).is_ok()
}
