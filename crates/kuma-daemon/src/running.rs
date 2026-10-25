//! The daemons running on this machine, for `kuma servers`.
//!
//! A daemon on 7455 can be found by asking 7455, and nothing else can: the desktop shell
//! puts every project after the first on an ephemeral port, and a `kuma mcp` that could not
//! have 7455 starts a private daemon on one. So each daemon leaves a note while it serves —
//! one small JSON file per address in a directory of this user's — and anyone wanting the
//! editor of a project in a browser reads them.
//!
//! A note is a claim, not a fact. It is removed when the daemon stops — on Ctrl-C, on the
//! `SIGTERM` the desktop shell sends — which a killed process never does, so [`notes`]
//! checks each one against the address it names and deletes the ones nothing answers for.
//! Nothing here is ever trusted to be current on its own, and failing to write a note costs
//! that listing only: serving never depends on it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One daemon's note: where it answers, and what it serves.
#[derive(Debug, Serialize, Deserialize)]
pub struct Running {
    /// Where a browser reaches its editor: `http://<ip>:<port>`, loopback for a daemon
    /// bound to every interface.
    pub url: String,
    /// The project file it serves, as it was given to the daemon.
    pub project: PathBuf,
    pub pid: u32,
    /// When it started serving, seconds since the Unix epoch.
    pub started: u64,
}

/// Where the notes are: `$XDG_STATE_HOME/kuma/daemons` (`~/.local/state`), or
/// `KUMA_DAEMONS_DIR` (tests point it elsewhere). Not `$XDG_RUNTIME_DIR`, though notes are
/// runtime state: it is set for a desktop session's processes and often not for an ssh
/// shell, cron or `sudo -u`, so the app's daemons and a CLI asking about them would look in
/// two places. A note left behind costs nothing; readers drop it.
fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("KUMA_DAEMONS_DIR") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("kuma/daemons")
}

/// The URL a browser on this machine uses for a daemon bound to `addr`.
pub fn url_for(addr: SocketAddr) -> String {
    let ip = match addr.ip() {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => std::net::Ipv4Addr::LOCALHOST.into(),
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => std::net::Ipv6Addr::LOCALHOST.into(),
        ip => ip,
    };
    format!("http://{}", SocketAddr::new(ip, addr.port()))
}

/// A note's file name. By address, so a daemon that takes over a port a killed one held
/// replaces its note rather than standing beside it.
fn file_name(addr: SocketAddr) -> String {
    format!("{}.json", addr.to_string().replace([':', '[', ']'], "_"))
}

/// While held, the note saying this daemon serves; dropping it removes the note, unless
/// another daemon has taken the address and written its own since.
pub struct Registered(PathBuf);

impl Drop for Registered {
    fn drop(&mut self) {
        remove_if_mine(&self.0);
    }
}

fn remove_if_mine(path: &Path) {
    if read(path).is_some_and(|r| r.pid == std::process::id()) {
        let _ = std::fs::remove_file(path);
    }
}

/// Leave a note in [`dir`] that this process serves `project` at `addr`. Best effort: a
/// directory that cannot be written is logged and the daemon serves regardless.
pub fn register(addr: SocketAddr, project: &Path) -> Option<Registered> {
    let dir = dir();
    register_in(&dir, addr, project).inspect_err(|e| tracing::warn!("cannot note this daemon in {} (`kuma servers` will not list it): {e:#}", dir.display())).ok()
}

fn register_in(dir: &Path, addr: SocketAddr, project: &Path) -> anyhow::Result<Registered> {
    let note = Running { url: url_for(addr), project: project.to_path_buf(), pid: std::process::id(), started: crate::history::now_ms() / 1000 };
    let path = dir.join(file_name(addr));
    // Written whole and renamed, so a reader never sees half a note.
    crate::jobs::write_atomic(&path, &serde_json::to_vec_pretty(&note)?)?;
    Ok(Registered(path))
}

/// Remove every note this process left, for a daemon about to exit without unwinding (a
/// signal handler), where no [`Registered`] will be dropped.
pub fn forget_mine() {
    for (path, _) in notes_in(&dir()) {
        remove_if_mine(&path);
    }
}

fn read(path: &Path) -> Option<Running> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Every note in [`dir`] that may still be a daemon, oldest first. One whose process has
/// gone, or whose address refuses connections, was killed: it is deleted here, so every
/// reader agrees on that much. A connection that only times out is a busy daemon (or a
/// busy machine), not a dead one, and its note stays. Whether what answers is still that
/// daemon is the reader's to ask (`/api/health`), since that takes an HTTP client this
/// crate does not have. The addresses are tried at once, so a slow one costs one timeout.
pub fn notes() -> Vec<(PathBuf, Running)> {
    let notes = notes_in(&dir());
    let dead: Vec<bool> = std::thread::scope(|s| {
        let checks: Vec<_> = notes.iter().map(|(_, r)| s.spawn(|| !alive(r.pid) || refused(&r.url))).collect();
        checks.into_iter().map(|c| c.join().unwrap_or(false)).collect()
    });
    notes
        .into_iter()
        .zip(dead)
        .filter_map(|(note, dead)| if dead { let _ = std::fs::remove_file(&note.0); None } else { Some(note) })
        .collect()
}

/// Whether process `pid` is running, where this machine can say (`/proc`); elsewhere it is
/// taken to be, and the address decides. A zombie is not: it has exited and only waits for
/// its parent to collect it, which a container's init can be slow to do, or never.
fn alive(pid: u32) -> bool {
    if !Path::new("/proc/self").exists() {
        return true;
    }
    // `<pid> (<name>) <state> …`; the name may hold anything, so the state follows its last `)`.
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.rsplit_once(')').is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z')))
}

/// Whether nothing listens at `url` any more — refused, not merely slow.
fn refused(url: &str) -> bool {
    let Ok(addr) = url.trim_start_matches("http://").parse::<SocketAddr>() else { return true };
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)).is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)
}

/// The notes in `dir`, unchecked, oldest first. A file that is not a note is skipped.
fn notes_in(dir: &Path) -> Vec<(PathBuf, Running)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json") && !p.file_name().is_some_and(|n| kuma_overlay::is_in_flight(&n.to_string_lossy())))
        .filter_map(|p| read(&p).map(|r| (p, r)))
        .collect();
    out.sort_by(|a, b| (a.1.started, &a.1.url).cmp(&(b.1.started, &b.1.url)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_is_noted_while_it_serves() {
        let tmp = tempfile::tempdir().unwrap();
        let addr: SocketAddr = "127.0.0.1:7455".parse().unwrap();
        let note = register_in(tmp.path(), addr, Path::new("/videos/a.kuma.json")).unwrap();
        let listed = notes_in(tmp.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.url, "http://127.0.0.1:7455");
        assert_eq!(listed[0].1.project, Path::new("/videos/a.kuma.json"));
        assert_eq!(listed[0].1.pid, std::process::id());
        drop(note);
        assert!(notes_in(tmp.path()).is_empty(), "a daemon that stops takes its note with it");
    }

    /// A daemon that takes a port over replaces the note of the one killed there, and the
    /// old one's guard, if it ever runs, does not remove the new note.
    #[test]
    fn the_newest_daemon_on_an_address_owns_its_note() {
        let tmp = tempfile::tempdir().unwrap();
        let addr: SocketAddr = "127.0.0.1:41000".parse().unwrap();
        let old = register_in(tmp.path(), addr, Path::new("/a.kuma.json")).unwrap();
        // Another process on the same port: only its pid differs from here.
        let path = tmp.path().join(file_name(addr));
        let mut theirs = read(&path).unwrap();
        theirs.pid += 1;
        theirs.project = PathBuf::from("/b.kuma.json");
        std::fs::write(&path, serde_json::to_vec(&theirs).unwrap()).unwrap();
        drop(old);
        let listed = notes_in(tmp.path());
        assert_eq!(listed.iter().map(|(_, r)| r.project.clone()).collect::<Vec<_>>(), vec![PathBuf::from("/b.kuma.json")]);
    }

    #[test]
    fn every_interface_is_reached_on_loopback() {
        assert_eq!(url_for("0.0.0.0:7455".parse().unwrap()), "http://127.0.0.1:7455");
        assert_eq!(url_for("[::]:7455".parse().unwrap()), "http://[::1]:7455");
        assert_eq!(url_for("192.168.1.4:80".parse().unwrap()), "http://192.168.1.4:80");
        assert_eq!(file_name("[::1]:7455".parse().unwrap()), "___1__7455.json");
    }

    /// A note whose process has gone, or whose port refuses, is dropped; one that is alive
    /// and listening stays.
    #[test]
    fn notes_of_dead_daemons_are_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let listening = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let refusing = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let live = register_in(tmp.path(), listening.local_addr().unwrap(), Path::new("/live.kuma.json")).unwrap();
        let _closed = register_in(tmp.path(), refusing, Path::new("/closed.kuma.json")).unwrap();
        // Listening, but by a process that is not the one the note names.
        let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let _gone = register_in(tmp.path(), other.local_addr().unwrap(), Path::new("/gone.kuma.json")).unwrap();
        let gone = tmp.path().join(file_name(other.local_addr().unwrap()));
        let mut note = read(&gone).unwrap();
        note.pid = u32::MAX;
        std::fs::write(&gone, serde_json::to_vec(&note).unwrap()).unwrap();

        std::env::set_var("KUMA_DAEMONS_DIR", tmp.path());
        let kept: Vec<_> = notes().into_iter().map(|(_, r)| r.project).collect();
        std::env::remove_var("KUMA_DAEMONS_DIR");
        let expected: Vec<PathBuf> = if Path::new("/proc/self").exists() { vec!["/live.kuma.json".into()] } else { vec!["/gone.kuma.json".into(), "/live.kuma.json".into()] };
        let mut kept = kept;
        kept.sort();
        assert_eq!(kept, expected);
        drop(live);
    }

    #[test]
    fn what_is_not_a_note_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("junk.json"), "{").unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "hi").unwrap();
        assert!(notes_in(tmp.path()).is_empty());
        assert!(notes_in(&tmp.path().join("missing")).is_empty());
    }
}
