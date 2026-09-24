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
        /// Sequence (bin id) to use instead of the project's master.
        #[arg(long)]
        sequence: Option<String>,
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
        /// Sequence (bin id) to use instead of the project's master.
        #[arg(long)]
        sequence: Option<String>,
        /// Sample one overlay bin entry on its own instead of a timeline — what the
        /// editor's overlay workbench shows. Times are seconds into the overlay.
        #[arg(long)]
        overlay: Option<String>,
        /// How long to run that overlay for (default: its manifest's own).
        #[arg(long)]
        duration: Option<f64>,
        /// Colour behind it, e.g. `#102030` or `transparent` (default black).
        #[arg(long)]
        backdrop: Option<String>,
        /// With --overlay: render it into another frame, `WxH` (e.g. 1080x1920), to check
        /// the orientation the project is not in.
        #[arg(long)]
        frame: Option<String>,
    },
    /// Show how big the project's `.kuma/` cache is, or prune it.
    Cache {
        #[command(subcommand)]
        action: Option<CacheCmd>,
    },
    /// Write the compiled MLT XML to stdout (overlays skipped unless baked).
    Compile {
        /// Sequence (bin id) to use instead of the project's master.
        #[arg(long)]
        sequence: Option<String>,
    },
    /// Validate the project and print issues (exit 1 on errors).
    Validate,
    /// Check the installation: repo, tools, engine, library, daemon, MCP config.
    Doctor,
    /// Scaffold a contract-abiding overlay folder in the project's `overlays/`. It is not
    /// in the bin until `kuma import-overlay` adds it.
    NewOverlay {
        /// Folder name: [A-Za-z_][A-Za-z0-9_-]*
        id: String,
        /// Display name (defaults to the id).
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// Write it to the user library as a blueprint instead of into this project.
        #[arg(long)]
        blueprint: bool,
    },
    /// Add an overlay folder of this project (`overlays/<name>`, or its `overlay.json`) to the bin.
    ImportOverlay {
        path: String,
        /// Bin id (minted from the folder name by default).
        #[arg(long)]
        id: Option<String>,
    },
    /// Check overlays against the contract: project overlay folders (a path holding an
    /// `overlay.json`) or blueprint ids; with none, every blueprint and every folder the bin uses.
    /// Exit 1 on errors.
    Lint { targets: Vec<String> },
    /// Generate `thumb.png` for blueprints (all, or the given ids).
    Thumbs { blueprints: Vec<String> },
    /// Copy a design system into the project for the agent to read (`design-system/`).
    /// With no id, list what is installed.
    DesignSystem {
        id: Option<String>,
        /// Replace the project's current one.
        #[arg(long)]
        force: bool,
    },
    /// Install the starter blueprints, presets, design systems and fonts into the user library (`~/.local/share/kuma`). Adds what is missing and new faces of installed fonts, always refreshes `_vendor/`; `--force` replaces the rest too.
    Library {
        /// Overwrite blueprints and presets that already exist in the library.
        #[arg(long)]
        force: bool,
    },
    /// Probe a media file and add it to the project bin.
    AddMedia {
        path: PathBuf,
        #[arg(long)]
        id: Option<String>,
    },
    /// Transcribe a media entry with whisper.cpp and add the subtitles to the bin.
    Transcribe {
        /// Bin id of the media entry (the result is added to the bin, unplaced).
        bin: Option<String>,
        /// Or a clip id: place the subtitles over that clip, aligned.
        #[arg(long, conflicts_with = "bin")]
        clip: Option<String>,
        /// Bin id for the subtitles entry. Left out, one is minted (`<bin>-subs`, then
        /// `<bin>-subs-2`…), so running twice adds a second transcript.
        #[arg(long)]
        id: Option<String>,
        /// Overwrite the transcript that is there (`--id`, else `<bin>-subs`), keeping its
        /// styling and placement.
        #[arg(long)]
        replace: bool,
        /// ISO 639-1 language code, or `auto`.
        #[arg(long)]
        language: Option<String>,
        /// Longest cue in characters: 1 (the default) is one cue per word, 0 keeps whisper's own sentences.
        #[arg(long, default_value_t = 1)]
        max_chars: u32,
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

#[derive(Subcommand)]
enum CacheCmd {
    /// Delete bakes nothing in the project uses, sampled frames, and thumbnails of entries
    /// no longer in the bin. Waits for renders and samples that are using the cache.
    Prune {
        /// Empty bakes and samples outright; the next preview or render bakes again.
        #[arg(long)]
        all: bool,
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
        Cmd::Render { preset, output, sequence } => {
            let state = open(&cli)?;
            let req = kuma_daemon::jobs::RenderRequest { preset: preset.clone(), output: output.as_deref().map(absolute).transpose()?, range: None, sequence: sequence.clone() };
            let out = tokio::task::spawn_blocking(move || kuma_daemon::jobs::render(&state, &req, &|p, m| eprintln!("[{p:3}%] {m}"), &Default::default())).await??;
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
        Cmd::Sample { t0, t1, count, rect, width, sequence, overlay, duration, backdrop, frame } => {
            let state = open(&cli)?;
            let rect = rect
                .as_deref()
                .map(|s| {
                    let v: Vec<u32> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
                    anyhow::ensure!(v.len() == 4, "rect must be x,y,w,h");
                    Ok::<_, anyhow::Error>(kuma_core::manifest::Region { x: v[0], y: v[1], w: v[2], h: v[3] })
                })
                .transpose()?;
            let frame = frame
                .as_deref()
                .map(|s| {
                    let (w, h) = s.split_once(['x', 'X', ',']).ok_or_else(|| anyhow::anyhow!("--frame takes WxH, e.g. 1080x1920"))?;
                    anyhow::Ok((w.trim().parse::<u32>()?, h.trim().parse::<u32>()?))
                })
                .transpose()?;
            let req = kuma_daemon::jobs::SampleRequest {
                t0: *t0,
                t1: *t1,
                count: *count,
                rect,
                width: *width,
                sequence: sequence.clone(),
                overlay: overlay.clone(),
                duration: *duration,
                backdrop: backdrop.clone(),
                frame_width: frame.map(|(w, _)| w),
                frame_height: frame.map(|(_, h)| h),
                ..Default::default()
            };
            let samples = tokio::task::spawn_blocking(move || kuma_daemon::jobs::sample_frames(&state, &req, &|_, _| {})).await??;
            println!("{}", serde_json::to_string_pretty(&samples)?);
        }
        Cmd::Cache { action } => {
            // Through the daemon when one runs: it knows what its renders and samples are
            // reading, and a prune from outside would not wait for them.
            let daemon = running_daemon(&cli).await.map(kuma_mcp::client::Client::new);
            match action {
                None => {
                    let usage = match &daemon {
                        Some(c) => serde_json::from_value(c.get("/api/cache").await?)?,
                        None => kuma_daemon::cache::usage(&*open(&cli)?)?,
                    };
                    for (name, part) in [("bakes", usage.bakes), ("samples", usage.samples), ("thumbs", usage.thumbs)] {
                        println!("{name:8} {:>10}  {} entries", human_bytes(part.bytes), part.entries);
                    }
                    println!("{:8} {:>10}", "total", human_bytes(usage.bytes));
                }
                Some(CacheCmd::Prune { all }) => {
                    let pruned: kuma_daemon::cache::Pruned = match &daemon {
                        Some(c) => {
                            let v = c.post("/api/cache/prune", &serde_json::json!({ "all": all })).await?;
                            let job = c.wait_job(v["job"].as_u64().unwrap_or_default(), std::time::Duration::from_secs(24 * 3600)).await?;
                            serde_json::from_value(job["result"].clone())?
                        }
                        None => {
                            let state = open(&cli)?;
                            let all = *all;
                            tokio::task::spawn_blocking(move || kuma_daemon::cache::prune(&state, all, &|_, m| eprintln!("{m}"))).await??
                        }
                    };
                    let (f, k) = (pruned.freed, pruned.kept);
                    println!("freed {} ({} bakes, {} samples, {} thumbnails); {} left", human_bytes(f.bytes), f.bakes.entries, f.samples.entries, f.thumbs.entries, human_bytes(k.bytes));
                }
            }
        }
        Cmd::Compile { sequence } => {
            let state = open(&cli)?;
            let project = state.project.read().unwrap().clone();
            let mut opts = kuma_daemon::jobs::compile_options(&state);
            opts.sequence = sequence.clone();
            print!("{}", kuma_daemon::jobs::compile_xml(&project, &opts)?);
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
        Cmd::NewOverlay { id, name, description, blueprint } => {
            let state = open(&cli)?;
            let dir = if *blueprint { state.config.user_blueprint_dir() } else { state.config.overlays_dir() };
            let out = kuma_overlay::new_overlay(&dir, id, name.as_deref().unwrap_or(""), description.as_deref().unwrap_or("")).map_err(anyhow::Error::msg)?;
            println!("{}", out.display());
            let report = kuma_daemon::overlays::lint(&state, &out)?;
            for line in report.lines() {
                println!("  {line}");
            }
            if !*blueprint {
                println!("add it to the bin with `kuma import-overlay overlays/{id}` once it is ready");
            }
        }
        Cmd::ImportOverlay { path, id } => {
            // Absolute and through the daemon when one runs, as `add-media` does.
            let path = absolute(Path::new(path))?.to_string_lossy().into_owned();
            let (id, rev) = if let Some(url) = running_daemon(&cli).await {
                let body = serde_json::json!({ "path": path, "id": id, "origin": "cli: import overlay" });
                let v = kuma_mcp::client::Client::new(&url).post("/api/overlays/import", &body).await?;
                (v["id"].as_str().unwrap_or_default().to_string(), v["revision"].as_u64().unwrap_or_default())
            } else {
                let state = open(&cli)?;
                let (id, rev, _) = kuma_daemon::overlays::import_overlay(&state, &path, kuma_daemon::overlays::Import { id: id.clone(), ..Default::default() }, None, "cli")?;
                (id, rev)
            };
            println!("added bin entry `{id}` (revision {rev})");
        }
        Cmd::Lint { targets } => {
            let state = open(&cli)?;
            // With no targets: every blueprint and every folder the bin uses.
            let targets: Vec<String> = if targets.is_empty() {
                let blueprints = state.blueprints.read().unwrap().loaded.keys().cloned().collect::<Vec<_>>();
                let folders: Vec<String> = state.project.read().unwrap().overlay_folders().into_iter().map(str::to_string).collect();
                blueprints.into_iter().chain(folders).collect()
            } else {
                targets.clone()
            };
            let mut errors = 0;
            for target in targets {
                let folder = state.config.project_root().join(&target).join(kuma_core::manifest::MANIFEST).is_file();
                let report = if folder { kuma_daemon::overlays::lint_overlay(&state, &target)? } else { kuma_daemon::overlays::lint_blueprint(&state, &target)? };
                errors += report.errors();
                let status = if report.ok() { "ok" } else { "FAILED" };
                println!("{target}: {status} ({} error(s), {} warning(s))", report.errors(), report.warnings());
                for line in report.lines() {
                    println!("  {line}");
                }
                if let Some(why) = &report.renderer_skipped {
                    println!("  (hyperframes' own linter did not run: {why})");
                }
            }
            if errors > 0 {
                std::process::exit(1);
            }
        }
        Cmd::Thumbs { blueprints } => {
            let state = open(&cli)?;
            let all = state.blueprints.read().unwrap().loaded.clone();
            let empty = serde_json::Map::new();
            for (id, (dir, manifest)) in all.iter().filter(|(id, _)| blueprints.is_empty() || blueprints.contains(id)) {
                let out = dir.join("thumb.png");
                state.baker.thumbnail(dir, manifest, &empty, &out).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
                println!("{id:16} {}", out.display());
            }
        }
        Cmd::DesignSystem { id, force } => {
            let state = open(&cli)?;
            let installed = state.config.installed_design_systems();
            let Some(id) = id else {
                if installed.is_empty() {
                    println!("no design systems installed — none ship yet; write one under `<library>/design-systems/<id>/` (`DESIGN.md` + `tokens.css`), or point KUMA_LIBRARY at a folder of them");
                }
                for (id, (dir, title)) in &installed {
                    let here = state.config.project_design_system().is_some_and(|p| std::fs::read_to_string(p).ok().as_deref() == std::fs::read_to_string(dir.join("DESIGN.md")).ok().as_deref());
                    println!("{id}\t{title}{}", if here { "  (this project's)" } else { "" });
                }
                return Ok(());
            };
            let (src, title) = installed.get(id).cloned().ok_or_else(|| anyhow::anyhow!("unknown design system `{id}` — `kuma design-system` lists what is installed"))?;
            // One fixed place, so the contract can say where it is and the agent can look
            // without being told: `<project>/design-system/`.
            let dst = state.config.project_root().join("design-system");
            if dst.exists() && !*force {
                anyhow::bail!("{} already exists — pass --force to replace it", dst.display());
            }
            if dst.exists() {
                std::fs::remove_dir_all(&dst)?;
            }
            kuma_daemon::copy_tree(&src, &dst)?;
            println!("{title} → {}", dst.display());
            println!("The agent reads `design-system/DESIGN.md`; overlays copy the `:root` block from `tokens.css`.");
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
                let (id, rev, _) = kuma_daemon::jobs::add_media(&state, &path, id.clone(), None, None, "cli")?;
                (id, rev)
            };
            println!("added bin entry `{id}` (revision {rev})");
        }
        Cmd::Transcribe { bin, clip, id, replace, language, max_chars } => {
            let options = kuma_overlay::TranscribeOptions { language: language.clone(), max_chars: Some(*max_chars) };
            let out = if let Some(url) = running_daemon(&cli).await {
                let body = serde_json::json!({ "bin": bin, "clip": clip, "id": id, "replace": replace, "language": options.language, "max_chars": options.max_chars, "origin": "cli: transcribe" });
                let v = kuma_mcp::client::Client::new(&url).post("/api/transcribe", &body).await?;
                kuma_mcp::client::Client::new(&url).wait_job(v["job"].as_u64().unwrap_or_default(), std::time::Duration::from_secs(3600)).await?
            } else {
                let state = open(&cli)?;
                let req = kuma_daemon::jobs::TranscribeRequest { bin: bin.clone(), clip: clip.clone(), id: id.clone(), replace: *replace, options, origin: "cli".into() };
                tokio::task::spawn_blocking(move || kuma_daemon::jobs::transcribe(&state, &req, &|p, m| eprintln!("[{p:3}%] {m}"))).await??
            };
            println!("{}", serde_json::to_string_pretty(&out)?);
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
            let fps = if fps.contains('/') {
                kuma_core::Fps::parse_ratio(fps).ok_or_else(|| anyhow::anyhow!("bad frame rate {fps:?}"))?
            } else {
                serde_json::from_str::<kuma_core::Fps>(fps)?
            };
            let p = kuma_core::Project::new_default(kuma_core::Project::name_from_path(&path), *width, *height, fps);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, serde_json::to_string_pretty(&p)?)?;
            println!("created {}", path.display());
            let library = default_library_dirs().remove(0);
            if !library.join("blueprints").is_dir() {
                println!("note: no blueprints at {} — run `kuma library` to install the starter set", library.display());
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
    for d in cfg.blueprint_dirs() {
        let (loaded, errors) = kuma_core::OverlayManifest::load_all(&d);
        println!("blueprints    {} — {} loaded{}", d.display(), loaded.len(), if errors.is_empty() { String::new() } else { format!(", {} failed: {}", errors.len(), errors[0]) });
    }
    println!(
        "design system {}",
        cfg.project_design_system()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("none in this project — `kuma design-system` lists {} installed", cfg.installed_design_systems().len()))
    );
    println!("ui            {}", ok(cfg.repo_root.join("ui/dist").is_dir(), "ui/dist", "cd ui && npm run build"));
    println!("engine        {}", ok(cfg.repo_root.join("engine/build/kuma-preview").is_file(), "engine/build/kuma-preview", "cmake -S engine -B engine/build && cmake --build engine/build -j"));
    println!("melt          {}", ok(kuma_mlt::melt::melt_available(), "ok", "apt install melt"));
    println!("ffprobe       {}", ok(kuma_overlay::media::ffprobe_available(), "ok", "apt install ffmpeg"));
    let baker = kuma_overlay::Baker::discover(&cfg.repo_root, PathBuf::new(), cfg.bakes_dir());
    println!("hyperframes   {}", baker.available().map(|v| format!("{v} ({})", baker.hyperframes.display())).unwrap_or_else(|e| format!("MISSING — npm install at the repo root ({e})")));
    let stt = kuma_overlay::Transcriber::discover(&cfg.repo_root, default_library_dirs().remove(0).join("models"));
    println!("whisper       {}", stt.available().map(|m| format!("{} with {}", stt.cli.display(), m.display())).unwrap_or_else(|e| format!("MISSING — {e}")));
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

/// Copy `<src>/{blueprints,presets,design-systems}` into the library, skipping entries that already exist
/// unless `force`. Returns the number of entries written.
fn install_library(src: &Path, dst: &Path, force: bool) -> anyhow::Result<usize> {
    let mut n = 0;
    for kind in ["blueprints", "presets", "design-systems", "fonts"] {
        let Ok(rd) = std::fs::read_dir(src.join(kind)) else { continue };
        for e in rd.flatten() {
            let target = dst.join(kind).join(e.file_name());
            let is_dir = e.path().is_dir();
            // `_vendor/` is kuma's runtime, not an asset of the user's: the blueprints just
            // installed need the one shipped with them, so it is always replaced.
            let runtime = kind == "blueprints" && e.file_name() == "_vendor";
            if target.exists() && !force && !runtime {
                // A font family already there takes the faces it lacks; anything else there
                // is left as it is, since it may be someone's edit.
                if kind == "fonts" && is_dir {
                    n += usize::from(kuma_daemon::add_missing(&e.path(), &target)? > 0);
                }
                continue;
            }
            if is_dir {
                kuma_daemon::copy_tree(&e.path(), &target)?;
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

/// `1.4 GB`, `312 MB`, `12 kB`: decimal, as file managers show it.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else if v < 10.0 { format!("{v:.1} {}", UNITS[unit]) } else { format!("{v:.0} {}", UNITS[unit]) }
}
