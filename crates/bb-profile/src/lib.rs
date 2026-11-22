//! `bb-profile`: the browsing profile on disk. One SQLite database, `profile.sqlite`, in the
//! profile directory holds:
//!
//! - **cookies**: persistent cookies (session cookies and private windows' cookies are never
//!   written), saved shortly after they change and at exit;
//! - **prefs**: string preferences (today the window size), written when set.
//!
//! The database is opened with an exclusive lock for the whole session, so a second browser
//! using the same profile notices (`OpenStatus::InUse`) and runs without saving instead of
//! overwriting the first one's data. A database that can't be read is moved aside
//! (`profile.sqlite.corrupt-<time>`) and a fresh one is created; a database from a newer
//! version of the browser is left untouched.

use bb_net::cookies::{Cookie, CookieJar, SameSite};
use rusqlite::{params, Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The schema version this build reads and writes (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 1;

/// The database file inside the profile directory.
pub const DB_FILE: &str = "profile.sqlite";

/// How often the cookie saver checks the jar for changes.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(2);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cookies (
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    domain TEXT NOT NULL,
    host_only INTEGER NOT NULL,
    path TEXT NOT NULL,
    expires INTEGER NOT NULL,
    secure INTEGER NOT NULL,
    http_only INTEGER NOT NULL,
    same_site TEXT NOT NULL,
    creation INTEGER NOT NULL,
    last_access INTEGER NOT NULL,
    PRIMARY KEY (name, domain, host_only, path)
);
CREATE TABLE IF NOT EXISTS prefs (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);";

/// How opening the profile went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenStatus {
    /// An existing profile was opened.
    Opened,
    /// A new profile was created.
    Created,
    /// The database couldn't be read; it was moved to this path and a new one created.
    Recovered(PathBuf),
    /// Another BannedBrowser is using this profile; this one runs without saving.
    InUse,
    /// The profile was written by a newer BannedBrowser (this schema version); it is left
    /// untouched and this one runs without saving.
    Newer(i64),
    /// The profile directory or database can't be used at all (the reason).
    Failed(String),
}

impl OpenStatus {
    /// A short code for logs and the GUI test state.
    pub fn code(&self) -> &'static str {
        match self {
            OpenStatus::Opened => "opened",
            OpenStatus::Created => "created",
            OpenStatus::Recovered(_) => "recovered",
            OpenStatus::InUse => "in-use",
            OpenStatus::Newer(_) => "newer",
            OpenStatus::Failed(_) => "failed",
        }
    }
}

/// An open profile.
pub struct Profile {
    dir: PathBuf,
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Profile").field("dir", &self.dir).finish()
    }
}

fn same_site_text(s: SameSite) -> &'static str {
    match s {
        SameSite::Strict => "strict",
        SameSite::Lax => "lax",
        SameSite::None => "none",
        SameSite::Default => "default",
    }
}

fn same_site_from(s: &str) -> SameSite {
    match s {
        "strict" => SameSite::Strict,
        "lax" => SameSite::Lax,
        "none" => SameSite::None,
        _ => SameSite::Default,
    }
}

/// Open the database, take the session's exclusive lock and check its version.
fn open_db(path: &Path) -> Result<(Connection, i64), rusqlite::Error> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(200))?;
    // Exclusive locking mode keeps the lock from the first write until the connection closes:
    // a second browser on the same profile then gets SQLITE_BUSY right here.
    conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    conn.execute_batch("BEGIN EXCLUSIVE; COMMIT;")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    Ok((conn, version))
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// Seconds since the Unix epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Profile {
    /// Open (or create) the profile in `dir`. `None` when it can't be used; the status says why.
    pub fn open(dir: &Path) -> (Option<Profile>, OpenStatus) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return (None, OpenStatus::Failed(format!("{}: {e}", dir.display())));
        }
        let path = dir.join(DB_FILE);
        let existed = path.exists();
        let mut status = if existed { OpenStatus::Opened } else { OpenStatus::Created };
        let opened = match open_db(&path) {
            Ok(x) => Ok(x),
            Err(e) if is_busy(&e) => return (None, OpenStatus::InUse),
            Err(e) => Err(e.to_string()),
        };
        let (conn, version) = match opened.and_then(|(c, v)| {
            // A quick structural check: a damaged file is moved aside instead of half-read.
            let ok: String =
                c.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            if ok == "ok" {
                Ok((c, v))
            } else {
                Err(format!("quick_check: {ok}"))
            }
        }) {
            Ok(x) => x,
            Err(why) => {
                log::warn!("profile database {} is unusable ({why}); starting a new one", path.display());
                let aside = dir.join(format!("{DB_FILE}.corrupt-{}", unix_now()));
                if let Err(e) = std::fs::rename(&path, &aside) {
                    return (None, OpenStatus::Failed(format!("can't move the damaged profile aside: {e}")));
                }
                for ext in ["-wal", "-shm", "-journal"] {
                    let _ = std::fs::remove_file(dir.join(format!("{DB_FILE}{ext}")));
                }
                status = OpenStatus::Recovered(aside);
                match open_db(&path) {
                    Ok(x) => x,
                    Err(e) if is_busy(&e) => return (None, OpenStatus::InUse),
                    Err(e) => return (None, OpenStatus::Failed(e.to_string())),
                }
            }
        };
        if version > SCHEMA_VERSION {
            return (None, OpenStatus::Newer(version));
        }
        let created =
            conn.execute_batch(SCHEMA).and_then(|_| conn.pragma_update(None, "user_version", SCHEMA_VERSION));
        if let Err(e) = created {
            return (None, OpenStatus::Failed(e.to_string()));
        }
        (Some(Profile { dir: dir.to_path_buf(), conn: Mutex::new(conn) }), status)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        let mut guard = match self.conn.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }

    /// The saved cookies that haven't expired (expired rows are deleted).
    pub fn load_cookies(&self) -> Vec<Cookie> {
        let now = bb_net::cookies::now();
        let r = self.with_conn(|c| {
            c.execute("DELETE FROM cookies WHERE expires <= ?1", params![now])?;
            let mut st = c.prepare(
                "SELECT name, value, domain, host_only, path, expires, secure, http_only, same_site, \
                 creation, last_access FROM cookies ORDER BY creation",
            )?;
            let rows = st.query_map([], |r| {
                Ok(Cookie {
                    name: r.get(0)?,
                    value: r.get(1)?,
                    domain: r.get(2)?,
                    host_only: r.get(3)?,
                    path: r.get(4)?,
                    expires: Some(r.get(5)?),
                    secure: r.get(6)?,
                    http_only: r.get(7)?,
                    same_site: same_site_from(&r.get::<_, String>(8)?),
                    creation: r.get(9)?,
                    last_access: r.get(10)?,
                    seq: 0,
                })
            })?;
            Ok(rows.filter_map(Result::ok).filter(valid_cookie).collect())
        });
        r.unwrap_or_else(|e| {
            log::warn!("couldn't load cookies: {e}");
            Vec::new()
        })
    }

    /// Replace the saved cookies with `cookies` (their persistent ones), in one transaction.
    pub fn save_cookies(&self, cookies: &[Cookie]) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM cookies", [])?;
            {
                let mut st = tx.prepare(
                    "INSERT OR REPLACE INTO cookies (name, value, domain, host_only, path, expires, secure, \
                     http_only, same_site, creation, last_access) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                )?;
                for k in cookies {
                    let Some(expires) = k.expires else { continue };
                    st.execute(params![
                        k.name,
                        k.value,
                        k.domain,
                        k.host_only,
                        k.path,
                        expires,
                        k.secure,
                        k.http_only,
                        same_site_text(k.same_site),
                        k.creation,
                        k.last_access
                    ])?;
                }
            }
            tx.commit()
        })
    }

    pub fn pref(&self, key: &str) -> Option<String> {
        self.with_conn(|c| {
            c.query_row("SELECT value FROM prefs WHERE key = ?1", params![key], |r| r.get(0)).or_else(|e| {
                match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(String::new()),
                    e => Err(e),
                }
            })
        })
        .ok()
        .filter(|v| !v.is_empty())
    }

    pub fn set_pref(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO prefs (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
    }
}

/// A row from disk is only trusted if it could have come from the jar: a non-empty domain, a
/// path starting with "/", no control characters.
fn valid_cookie(c: &Cookie) -> bool {
    let clean = |s: &str| !s.chars().any(|ch| ch.is_control() && ch != '\t');
    !c.domain.is_empty()
        && c.path.starts_with('/')
        && clean(&c.name)
        && clean(&c.value)
        && clean(&c.domain)
        && clean(&c.path)
        && !(c.name.is_empty() && c.value.is_empty())
}

/// Saves a cookie jar's persistent cookies to a profile in the background: shortly after they
/// change, and a last time when dropped (or [`CookieSaver::finish`]ed).
pub struct CookieSaver {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl CookieSaver {
    pub fn start(profile: Arc<Profile>, jar: Arc<dyn JarSource>) -> CookieSaver {
        let (tx, rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("cookie-saver".into())
            .spawn(move || {
                let mut saved = jar.jar().generation();
                loop {
                    let stop = match rx.recv_timeout(SAVE_INTERVAL) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
                        Err(RecvTimeoutError::Timeout) => false,
                    };
                    let g = jar.jar().generation();
                    // At exit, save even without changes: last-access times moved.
                    if g != saved || stop {
                        match profile.save_cookies(&jar.jar().persistent()) {
                            Ok(()) => saved = g,
                            Err(e) => log::warn!("couldn't save cookies: {e}"),
                        }
                    }
                    if stop {
                        break;
                    }
                }
            })
            .ok();
        CookieSaver { stop: Some(tx), thread }
    }

    /// Save once more and stop.
    pub fn finish(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CookieSaver {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// Where the saver finds the jar (the network client owns it).
pub trait JarSource: Send + Sync + 'static {
    fn jar(&self) -> &CookieJar;
}

impl JarSource for bb_net::Client {
    fn jar(&self) -> &CookieJar {
        self.cookies()
    }
}

impl JarSource for CookieJar {
    fn jar(&self) -> &CookieJar {
        self
    }
}
