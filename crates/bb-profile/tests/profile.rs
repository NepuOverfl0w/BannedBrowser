//! Profile storage: creating, reopening, locking, recovery from damage, newer schema
//! versions, cookies (persistent only, expiry, validation) and prefs.

use bb_net::cookies::{CookieContext, CookieJar};
use bb_profile::{CookieSaver, OpenStatus, Profile, DB_FILE};
use bb_url::Url;
use std::path::PathBuf;
use std::sync::Arc;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bb-profile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

#[test]
fn create_reopen_and_prefs() {
    let dir = temp("prefs");
    let (p, st) = Profile::open(&dir);
    assert_eq!(st, OpenStatus::Created);
    let p = p.unwrap();
    assert_eq!(p.pref("window.size"), None);
    p.set_pref("window.size", "900x600").unwrap();
    p.set_pref("window.size", "1000x700").unwrap();
    drop(p);
    let (p, st) = Profile::open(&dir);
    assert_eq!(st, OpenStatus::Opened);
    assert_eq!(p.unwrap().pref("window.size").as_deref(), Some("1000x700"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_browser_on_the_same_profile_is_told_it_is_in_use() {
    let dir = temp("lock");
    let (first, _) = Profile::open(&dir);
    assert!(first.is_some());
    let (second, st) = Profile::open(&dir);
    assert!(second.is_none());
    assert_eq!(st, OpenStatus::InUse);
    drop(first);
    let (third, st) = Profile::open(&dir);
    assert!(third.is_some(), "{st:?}");
    drop(third);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn damaged_databases_are_moved_aside() {
    let dir = temp("corrupt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(DB_FILE), b"this is not a database, just some bytes that look wrong").unwrap();
    let (p, st) = Profile::open(&dir);
    let OpenStatus::Recovered(aside) = st else { panic!("{st:?}") };
    assert!(aside.exists());
    assert!(std::fs::read(&aside).unwrap().starts_with(b"this is not"));
    let p = p.expect("a fresh profile");
    p.set_pref("k", "v").unwrap();
    assert_eq!(p.pref("k").as_deref(), Some("v"));
    drop(p);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_newer_schema_is_left_alone() {
    let dir = temp("newer");
    let (p, _) = Profile::open(&dir);
    p.unwrap().set_pref("keep", "me").unwrap();
    rusqlite_set_version(&dir.join(DB_FILE), 99);
    let before = std::fs::read(dir.join(DB_FILE)).unwrap();
    let (p, st) = Profile::open(&dir);
    assert!(p.is_none());
    assert_eq!(st, OpenStatus::Newer(99));
    assert_eq!(std::fs::read(dir.join(DB_FILE)).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

fn rusqlite_set_version(path: &std::path::Path, v: i64) {
    // Use the profile's own SQLite through a raw connection.
    let c = rusqlite::Connection::open(path).unwrap();
    c.pragma_update(None, "user_version", v).unwrap();
}

#[test]
fn persistent_cookies_survive_a_restart_session_cookies_dont() {
    let dir = temp("cookies");
    let ctx = CookieContext::default();
    let url = u("https://www.example.com/a/b");
    {
        let (p, _) = Profile::open(&dir);
        let p = Arc::new(p.unwrap());
        let jar = Arc::new(CookieJar::new());
        let saver = CookieSaver::start(p.clone(), jar.clone());
        assert!(jar.store(&url, "keep=1; Max-Age=3600; Path=/; Secure; HttpOnly; SameSite=Strict", &ctx));
        assert!(jar.store(&url, "session=1", &ctx));
        assert!(jar.store(&url, "short=1; Max-Age=3600", &ctx));
        assert!(jar.store(&url, "short=1; Max-Age=0", &ctx), "deleted again");
        saver.finish();
    }
    let (p, _) = Profile::open(&dir);
    let p = p.unwrap();
    let loaded = p.load_cookies();
    assert_eq!(loaded.len(), 1, "{loaded:?}");
    let k = &loaded[0];
    assert_eq!(
        (k.name.as_str(), k.value.as_str(), k.domain.as_str(), k.path.as_str()),
        ("keep", "1", "www.example.com", "/")
    );
    assert!(k.secure && k.http_only && k.host_only);
    assert_eq!(k.same_site, bb_net::cookies::SameSite::Strict);
    let jar = CookieJar::new();
    jar.load(loaded);
    assert_eq!(jar.cookie_header(&url, &ctx).as_deref(), Some("keep=1"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expired_and_invalid_rows_are_dropped() {
    let dir = temp("rows");
    let (p, _) = Profile::open(&dir);
    drop(p);
    {
        let c = rusqlite::Connection::open(dir.join(DB_FILE)).unwrap();
        let now = bb_net::cookies::now();
        for (name, domain, path, expires) in [
            ("ok", "a.test", "/", now + 100),
            ("old", "a.test", "/", now - 1),
            ("bad\u{1}", "a.test", "/", now + 100),
            ("nodomain", "", "/", now + 100),
            ("relpath", "a.test", "x", now + 100),
        ] {
            c.execute(
                "INSERT INTO cookies VALUES (?1, 'v', ?2, 1, ?3, ?4, 0, 0, 'lax', 0, 0)",
                rusqlite::params![name, domain, path, expires],
            )
            .unwrap();
        }
    }
    let (p, _) = Profile::open(&dir);
    let names: Vec<String> = p.unwrap().load_cookies().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["ok"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unusable_directories_fail_cleanly() {
    let dir = temp("file");
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    std::fs::write(&dir, b"a file, not a directory").unwrap();
    let (p, st) = Profile::open(&dir);
    assert!(p.is_none());
    assert_eq!(st.code(), "failed");
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn local_storage_survives_a_restart() {
    let dir = temp("storage");
    {
        let (p, _) = Profile::open(&dir);
        let p = Arc::new(p.unwrap());
        let client = Arc::new(bb_net::Client::new(bb_net::ClientConfig::default()));
        let saver = CookieSaver::start(p.clone(), client.clone());
        client
            .storage()
            .set_area("https://example.com", vec![("k".into(), "v".into()), ("x".into(), "y".into())]);
        client.storage().set_area("https://other.example", vec![("a".into(), "b".into())]);
        saver.finish();
    }
    let (p, _) = Profile::open(&dir);
    let p = p.unwrap();
    let s = bb_net::storage::WebStorage::new();
    s.load(p.load_storage());
    assert_eq!(
        s.area("https://example.com"),
        vec![("k".to_string(), "v".to_string()), ("x".to_string(), "y".to_string())]
    );
    assert_eq!(s.area("https://other.example"), vec![("a".to_string(), "b".to_string())]);
    let _ = std::fs::remove_dir_all(&dir);
}
