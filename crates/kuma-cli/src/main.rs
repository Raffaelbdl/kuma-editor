use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use kuma_daemon::{Config, find_repo_root};

#[derive(Parser)]
#[command(name = "kuma", version, about = "Hybrid video editor: MLT timeline + HTML overlays + MCP")]
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
        #[arg(default_value = None)]
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
    /// Create a new empty project file.
    Init {
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value = "30")]
        fps: String,
    },
}

fn config(cli: &Cli, bind: SocketAddr) -> anyhow::Result<Config> {
    let repo_root = cli
        .repo
        .clone()
        .or_else(find_repo_root)
        .ok_or_else(|| anyhow::anyhow!("cannot find the kuma repository root (overlays/); pass --repo or set KUMA_REPO"))?;
    let project_file = if cli.project.is_absolute() { cli.project.clone() } else { std::env::current_dir()?.join(&cli.project) };
    let ui_dir = repo_root.join("ui/dist");
    let engine = repo_root.join("engine/build/kuma-preview");
    Ok(Config {
        project_file,
        repo_root,
        ui_dir: ui_dir.is_dir().then_some(ui_dir),
        bind,
        preview_engine: engine.is_file().then_some(engine),
    })
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
            let url = match daemon {
                Some(u) => u.clone(),
                None => {
                    let cfg = config(&cli, *bind)?;
                    let url = format!("http://{}", cfg.bind);
                    // Reuse a running daemon on that port if there is one.
                    let running = reqwest_get_ok(&format!("{url}/api/health")).await;
                    if !running {
                        let (_state, addr) = kuma_daemon::serve(cfg).await?;
                        tracing::info!("started daemon at http://{addr}");
                    }
                    url
                }
            };
            kuma_mcp::run_stdio(url).await?;
        }
        Cmd::Render { preset, output, supersample } => {
            let cfg = config(&cli, "127.0.0.1:0".parse().unwrap())?;
            let state = std::sync::Arc::new(kuma_daemon::AppState::load(cfg)?);
            let req = kuma_daemon::jobs::RenderRequest { preset: preset.clone(), output: output.clone(), range: None, supersample: *supersample };
            let st = state.clone();
            let out = tokio::task::spawn_blocking(move || kuma_daemon::jobs::render(&st, &req, &|p, m| eprintln!("[{p:3}%] {m}"))).await??;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::Bake { clips, supersample } => {
            let cfg = config(&cli, "127.0.0.1:0".parse().unwrap())?;
            let state = std::sync::Arc::new(kuma_daemon::AppState::load(cfg)?);
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
            let cfg = config(&cli, "127.0.0.1:0".parse().unwrap())?;
            let state = std::sync::Arc::new(kuma_daemon::AppState::load(cfg)?);
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
            let cfg = config(&cli, "127.0.0.1:0".parse().unwrap())?;
            let state = kuma_daemon::AppState::load(cfg)?;
            let project = state.project.read().unwrap().clone();
            let path = kuma_daemon::jobs::compile_project(&state, &project, Default::default(), "inspect", kuma_mlt::UnbakedPolicy::Skip, None)?;
            print!("{}", std::fs::read_to_string(path)?);
        }
        Cmd::Validate => {
            let cfg = config(&cli, "127.0.0.1:0".parse().unwrap())?;
            let state = kuma_daemon::AppState::load(cfg)?;
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
            let cfg = config(&cli, "127.0.0.1:0".parse().unwrap())?;
            let state = kuma_daemon::AppState::load(cfg)?;
            for t in state.template_infos() {
                println!("{:16} {:24} vars={}", t.id, t.manifest.name, t.manifest.vars.keys().cloned().collect::<Vec<_>>().join(","));
            }
        }
        Cmd::Thumbs { templates } => {
            let cfg = config(&cli, "127.0.0.1:0".parse().unwrap())?;
            let state = kuma_daemon::AppState::load(cfg)?;
            let project = state.project.read().unwrap().clone();
            let all = state.templates.read().unwrap().clone();
            for (id, (dir, manifest)) in all.iter().filter(|(id, _)| templates.is_empty() || templates.contains(id)) {
                let started = std::time::Instant::now();
                let out = state.baker.thumbnail(dir, manifest, project.width, project.height).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
                println!("{id:16} {} ({:.1}s)", out.display(), started.elapsed().as_secs_f64());
            }
        }
        Cmd::AddAsset { path, id } => {
            let cfg = config(&cli, "127.0.0.1:0".parse().unwrap())?;
            let state = kuma_daemon::AppState::load(cfg)?;
            let abs = kuma_daemon::jobs::resolve_path(&state, path);
            let probe = kuma_overlay::probe(&abs).map_err(|e| anyhow::anyhow!(e))?;
            let root = state.config.project_root();
            let rel = abs.strip_prefix(&root).map(|p| p.to_path_buf()).unwrap_or(abs.clone());
            let id = id.clone().unwrap_or_else(|| abs.file_stem().unwrap().to_string_lossy().to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric(), "_"));
            let asset = kuma_core::Asset { path: rel.to_string_lossy().into_owned(), kind: probe.kind, proxy: None, meta: Some(probe.meta) };
            let patch: json_patch::Patch = serde_json::from_value(serde_json::json!([{ "op": "add", "path": format!("/assets/{id}"), "value": asset }]))?;
            let (rev, _) = state.apply_patch(&patch.0, None, "cli")?;
            println!("added asset `{id}` (revision {rev})");
        }
        Cmd::Init { width, height, fps } => {
            let path = if cli.project.is_absolute() { cli.project.clone() } else { std::env::current_dir()?.join(&cli.project) };
            anyhow::ensure!(!path.exists(), "{} already exists", path.display());
            let mut p = kuma_core::Project::default();
            p.name = path.file_stem().map(|s| s.to_string_lossy().trim_end_matches(".kuma").to_string()).unwrap_or_default();
            p.width = *width;
            p.height = *height;
            // Accepts `30`, `29.97`, `30000/1001` or `{"num":30000,"den":1001}`.
            p.fps = if let Some((n, d)) = fps.split_once('/') {
                kuma_core::Fps::new(n.trim().parse()?, d.trim().parse()?)
            } else {
                serde_json::from_str::<kuma_core::Fps>(fps)?
            };
            p.tracks = vec![
                kuma_core::Track { id: "v1".into(), kind: kuma_core::TrackKind::Video, name: "Video".into(), muted: false, hidden: false, clips: vec![], transitions: vec![] },
                kuma_core::Track { id: "o1".into(), kind: kuma_core::TrackKind::Overlay, name: "Overlays".into(), muted: false, hidden: false, clips: vec![], transitions: vec![] },
                kuma_core::Track { id: "a1".into(), kind: kuma_core::TrackKind::Audio, name: "Audio".into(), muted: false, hidden: false, clips: vec![], transitions: vec![] },
            ];
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, serde_json::to_string_pretty(&p)?)?;
            println!("created {}", path.display());
        }
    }
    Ok(())
}

async fn reqwest_get_ok(url: &str) -> bool {
    kuma_mcp::client::health(url).await
}
