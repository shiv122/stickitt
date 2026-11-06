//! SQLite persistence: notes, the lists they belong to, profiles (named sets of active lists)
//! and small settings. Note writes are batched into a single transaction.

use crate::reminder::{Reminder, Repeat};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct NoteRecord {
    pub id: i64,
    pub body: String,
    /// Outer position in physical pixels (None → let the OS place it).
    pub pos: Option<(i32, i32)>,
    /// Inner size in logical pixels.
    pub size: (f64, f64),
    /// A preset index or `theme::CUSTOM + hue`.
    pub color: u16,
    pub mode: u8,
    pub zoom: u16,
    pub reminder: Option<Reminder>,
    /// Page icon (an emoji), empty for none.
    pub icon: String,
    pub list: i64,
    /// Virtual desktop the note lives on (a GUID string), when known.
    pub desktop: Option<String>,
    /// Closed with ×: kept (and listed in the dock) but not shown until reopened.
    pub hidden: bool,
    /// How the note is shown: 0 = full page, 1 = compact (one line).
    pub view: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub id: i64,
    pub name: String,
    /// Lists shown (and notes visible) while this profile is active.
    pub lists: Vec<i64>,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA_VERSION: i32 = 5;
/// The list every note falls back to; it can be renamed but not deleted.
pub const DEFAULT_LIST: i64 = 1;
/// The profile showing every list; created on first run.
pub const DEFAULT_PROFILE: i64 = 1;

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE notes (
                    id INTEGER PRIMARY KEY,
                    body TEXT NOT NULL DEFAULT '',
                    x INTEGER, y INTEGER,
                    w REAL NOT NULL, h REAL NOT NULL,
                    color INTEGER NOT NULL DEFAULT 0,
                    mode INTEGER NOT NULL DEFAULT 0,
                    zoom INTEGER NOT NULL DEFAULT 100,
                    reminder_due INTEGER,
                    reminder_anchor INTEGER,
                    reminder_repeat TEXT,
                    deleted_at INTEGER,
                    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                );",
            )?;
        }
        if version < 2 {
            conn.execute_batch("ALTER TABLE notes ADD COLUMN icon TEXT NOT NULL DEFAULT '';")?;
        }
        if version < 3 {
            conn.execute_batch(
                "CREATE TABLE lists (id INTEGER PRIMARY KEY, name TEXT NOT NULL, position INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE profiles (id INTEGER PRIMARY KEY, name TEXT NOT NULL, position INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE profile_lists (
                    profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
                    list_id INTEGER NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
                    PRIMARY KEY (profile_id, list_id)
                 );
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO lists (id, name) VALUES (1, 'Notes');
                 INSERT INTO profiles (id, name) VALUES (1, 'All');
                 INSERT INTO profile_lists VALUES (1, 1);
                 ALTER TABLE notes ADD COLUMN list_id INTEGER NOT NULL DEFAULT 1;
                 ALTER TABLE notes ADD COLUMN desktop TEXT;",
            )?;
        }
        if version < 4 {
            conn.execute_batch("ALTER TABLE notes ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;")?;
        }
        if version < 5 {
            conn.execute_batch("ALTER TABLE notes ADD COLUMN view INTEGER NOT NULL DEFAULT 0;")?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
    }

    // ---- notes ------------------------------------------------------------------------------

    /// Notes that are not in the trash.
    pub fn load(&self) -> rusqlite::Result<Vec<NoteRecord>> {
        self.notes_where("deleted_at IS NULL ORDER BY id", [])
    }

    /// The trash, most recently deleted first.
    pub fn trashed(&self, limit: i64) -> rusqlite::Result<Vec<NoteRecord>> {
        self.notes_where("deleted_at IS NOT NULL ORDER BY deleted_at DESC, id DESC LIMIT ?1", [limit])
    }

    fn notes_where<P: rusqlite::Params>(&self, filter: &str, args: P) -> rusqlite::Result<Vec<NoteRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, body, x, y, w, h, color, mode, zoom,
                    reminder_due, reminder_anchor, reminder_repeat, icon, list_id, desktop, hidden, view
             FROM notes WHERE {filter}"
        ))?;
        let rows = stmt.query_map(args, |r| {
            let x: Option<i32> = r.get(2)?;
            let y: Option<i32> = r.get(3)?;
            let due: Option<i64> = r.get(9)?;
            let anchor: Option<i64> = r.get(10)?;
            let repeat: Option<String> = r.get(11)?;
            Ok(NoteRecord {
                id: r.get(0)?,
                body: r.get(1)?,
                pos: x.zip(y),
                size: (r.get(4)?, r.get(5)?),
                color: r.get(6)?,
                mode: r.get(7)?,
                zoom: r.get(8)?,
                reminder: due.map(|due| Reminder {
                    due,
                    anchor: anchor.unwrap_or(due),
                    repeat: Repeat::decode(repeat.as_deref().unwrap_or("")),
                }),
                icon: r.get(12)?,
                list: r.get(13)?,
                desktop: r.get(14)?,
                hidden: r.get(15)?,
                view: r.get(16)?,
            })
        })?;
        rows.collect()
    }

    pub fn insert(&self, rec: &NoteRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO notes (body, w, h, color, mode, zoom, icon, list_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![rec.body, rec.size.0, rec.size.1, rec.color, rec.mode, rec.zoom, rec.icon, rec.list],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn save_all<'a>(&mut self, recs: impl IntoIterator<Item = &'a NoteRecord>) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE notes SET body=?2, x=?3, y=?4, w=?5, h=?6, color=?7, mode=?8, zoom=?9,
                    reminder_due=?10, reminder_anchor=?11, reminder_repeat=?12, icon=?13, list_id=?14,
                    desktop=COALESCE(?15, desktop), hidden=?16, view=?17, updated_at=unixepoch()
                 WHERE id=?1",
            )?;
            for r in recs {
                let rem = r.reminder.as_ref();
                stmt.execute(params![
                    r.id,
                    r.body,
                    r.pos.map(|p| p.0),
                    r.pos.map(|p| p.1),
                    r.size.0,
                    r.size.1,
                    r.color,
                    r.mode,
                    r.zoom,
                    rem.map(|r| r.due),
                    rem.map(|r| r.anchor),
                    rem.map(|r| r.repeat.encode()),
                    r.icon,
                    r.list,
                    r.desktop,
                    r.hidden,
                    r.view,
                ])?;
            }
        }
        tx.commit()
    }

    /// Soft delete: the note goes to the trash (restorable from the manager later).
    pub fn trash(&self, id: i64) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE notes SET deleted_at=unixepoch() WHERE id=?1", [id])?;
        Ok(())
    }

    /// Takes a note out of the trash and returns it.
    pub fn restore(&self, id: i64) -> rusqlite::Result<Option<NoteRecord>> {
        self.conn.execute("UPDATE notes SET deleted_at=NULL WHERE id=?1", [id])?;
        Ok(self.notes_where("id=?1", [id])?.pop())
    }

    /// Deletes one trashed note for good.
    pub fn purge(&self, id: i64) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM notes WHERE id=?1 AND deleted_at IS NOT NULL", [id])?;
        Ok(())
    }

    /// Deletes trashed notes for good: all of them, or those trashed more than `age` seconds ago.
    pub fn empty_trash(&self, age: Option<i64>) -> rusqlite::Result<usize> {
        match age {
            None => self.conn.execute("DELETE FROM notes WHERE deleted_at IS NOT NULL", []),
            Some(age) => self
                .conn
                .execute("DELETE FROM notes WHERE deleted_at IS NOT NULL AND deleted_at < unixepoch() - ?1", [age]),
        }
    }

    // ---- lists & profiles -------------------------------------------------------------------

    pub fn lists(&self) -> rusqlite::Result<Vec<List>> {
        let mut stmt = self.conn.prepare("SELECT id, name FROM lists ORDER BY position, id")?;
        let rows = stmt.query_map([], |r| Ok(List { id: r.get(0)?, name: r.get(1)? }))?;
        rows.collect()
    }

    pub fn profiles(&self) -> rusqlite::Result<Vec<Profile>> {
        let mut stmt = self.conn.prepare("SELECT id, name FROM profiles ORDER BY position, id")?;
        let mut profiles: Vec<Profile> = stmt
            .query_map([], |r| Ok(Profile { id: r.get(0)?, name: r.get(1)?, lists: Vec::new() }))?
            .collect::<rusqlite::Result<_>>()?;
        let mut links = self.conn.prepare(
            "SELECT pl.list_id FROM profile_lists pl JOIN lists l ON l.id = pl.list_id
             WHERE pl.profile_id = ?1 ORDER BY l.position, l.id",
        )?;
        for p in &mut profiles {
            p.lists = links.query_map([p.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        }
        Ok(profiles)
    }

    /// Creates a list and adds it to `profile` so it shows up right away.
    pub fn create_list(&self, name: &str, profile: i64) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO lists (name, position) VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM lists))",
            [name],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute("INSERT OR IGNORE INTO profile_lists VALUES (?1, ?2)", [profile, id])?;
        // The catch-all profile always shows every list.
        self.conn.execute("INSERT OR IGNORE INTO profile_lists VALUES (?1, ?2)", [DEFAULT_PROFILE, id])?;
        Ok(id)
    }

    /// Deletes a list. Its notes move to the default list, and every profile that showed the
    /// list now shows the default list too, so they stay in sight. With `trash_notes` they go to
    /// the trash instead (and come back into the default list if restored). Either way no
    /// profile is left showing no list at all.
    pub fn delete_list(&self, id: i64, trash_notes: bool) -> rusqlite::Result<()> {
        if id == DEFAULT_LIST {
            return Ok(());
        }
        if trash_notes {
            self.conn
                .execute("UPDATE notes SET deleted_at = unixepoch() WHERE list_id = ?1 AND deleted_at IS NULL", [id])?;
        }
        self.conn.execute("UPDATE notes SET list_id = ?1 WHERE list_id = ?2", [DEFAULT_LIST, id])?;
        let shown_by = if trash_notes {
            // Only the profiles that would be left empty.
            "SELECT profile_id, ?1 FROM profile_lists pl WHERE list_id = ?2 AND NOT EXISTS
             (SELECT 1 FROM profile_lists o WHERE o.profile_id = pl.profile_id AND o.list_id != ?2)"
        } else {
            "SELECT profile_id, ?1 FROM profile_lists WHERE list_id = ?2"
        };
        self.conn.execute(&format!("INSERT OR IGNORE INTO profile_lists {shown_by}"), [DEFAULT_LIST, id])?;
        self.conn.execute("DELETE FROM profile_lists WHERE list_id = ?1", [id])?;
        self.conn.execute("DELETE FROM lists WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Creates a profile showing `lists`.
    pub fn create_profile(&self, name: &str, lists: &[i64]) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO profiles (name, position) VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM profiles))",
            [name],
        )?;
        let id = self.conn.last_insert_rowid();
        for l in lists {
            self.conn.execute("INSERT OR IGNORE INTO profile_lists VALUES (?1, ?2)", [id, *l])?;
        }
        Ok(id)
    }

    pub fn delete_profile(&self, id: i64) -> rusqlite::Result<()> {
        if id != DEFAULT_PROFILE {
            self.conn.execute("DELETE FROM profiles WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    /// Shows or hides `list` in `profile`. A profile always shows at least one list: hiding the
    /// last one shows the default list instead (and the default list can't be the one hidden).
    pub fn set_profile_list(&self, profile: i64, list: i64, on: bool) -> rusqlite::Result<()> {
        if on {
            self.conn.execute("INSERT OR IGNORE INTO profile_lists VALUES (?1, ?2)", [profile, list])?;
            return Ok(());
        }
        let others: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM profile_lists WHERE profile_id = ?1 AND list_id != ?2",
            [profile, list],
            |r| r.get(0),
        )?;
        if others == 0 {
            if list == DEFAULT_LIST {
                return Ok(());
            }
            self.conn.execute("INSERT OR IGNORE INTO profile_lists VALUES (?1, ?2)", [profile, DEFAULT_LIST])?;
        }
        self.conn.execute("DELETE FROM profile_lists WHERE profile_id = ?1 AND list_id = ?2", [profile, list])?;
        Ok(())
    }

    // ---- settings ---------------------------------------------------------------------------

    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        );
    }

    #[cfg(test)]
    pub fn count(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM notes WHERE deleted_at IS NULL", [], |r| r.get(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_and_restore() {
        let store = Store::in_memory().unwrap();
        let a = store.insert(&note()).unwrap();
        let b = store.insert(&note()).unwrap();
        store.trash(a).unwrap();
        store.trash(b).unwrap();
        assert_eq!(store.load().unwrap().len(), 0);
        assert_eq!(store.trashed(10).unwrap().len(), 2);
        let back = store.restore(a).unwrap().unwrap();
        assert_eq!((back.id, back.icon.as_str()), (a, "📌"));
        assert_eq!(store.load().unwrap().len(), 1);
        // Just trashed, so not old enough to purge yet.
        assert_eq!(store.empty_trash(Some(3600)).unwrap(), 0);
        // Purging only touches the trash.
        store.purge(a).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
        assert_eq!(store.empty_trash(None).unwrap(), 1);
        assert!(store.trashed(10).unwrap().is_empty());
        assert_eq!(store.load().unwrap().len(), 1);
    }

    fn note() -> NoteRecord {
        NoteRecord {
            id: 0,
            body: "hi".into(),
            pos: None,
            size: (260.0, 240.0),
            color: 2,
            mode: 1,
            zoom: 100,
            reminder: None,
            icon: "📌".into(),
            list: DEFAULT_LIST,
            desktop: None,
            hidden: false,
            view: 0,
        }
    }

    #[test]
    fn roundtrip() {
        let mut store = Store::in_memory().unwrap();
        let mut rec = note();
        rec.id = store.insert(&rec).unwrap();
        rec.pos = Some((10, -20));
        rec.reminder = Some(Reminder { due: 5, anchor: 4, repeat: Repeat::Every(60) });
        rec.desktop = Some("{A}".into());
        rec.hidden = true;
        rec.view = 1;
        store.save_all([&rec]).unwrap();
        // A save without a known desktop keeps the stored one.
        rec.desktop = None;
        store.save_all([&rec]).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].pos, Some((10, -20)));
        assert_eq!(loaded[0].reminder, rec.reminder);
        assert_eq!(loaded[0].icon, "📌");
        assert_eq!(loaded[0].desktop.as_deref(), Some("{A}"));
        assert!(loaded[0].hidden);
        assert_eq!(loaded[0].view, 1);
        store.trash(rec.id).unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn lists_and_profiles() {
        let mut store = Store::in_memory().unwrap();
        let work = store.create_list("Work", DEFAULT_PROFILE).unwrap();
        let focus = store.create_profile("Focus", &[work]).unwrap();
        let profiles = store.profiles().unwrap();
        assert_eq!(profiles[0].lists, vec![DEFAULT_LIST, work]);
        assert_eq!(profiles[1], Profile { id: focus, name: "Focus".into(), lists: vec![work] });

        let mut rec = note();
        rec.list = work;
        rec.id = store.insert(&rec).unwrap();
        store.save_all([&rec]).unwrap();
        store.delete_list(work, false).unwrap();
        assert_eq!(store.load().unwrap()[0].list, DEFAULT_LIST);
        // The profile that showed the list now shows where its notes went.
        assert_eq!(store.profiles().unwrap()[1].lists, vec![DEFAULT_LIST]);

        store.set_setting("profile", "2");
        store.set_setting("profile", "3");
        assert_eq!(store.setting("profile").as_deref(), Some("3"));
    }

    #[test]
    fn deleting_a_list_with_its_notes() {
        let store = Store::in_memory().unwrap();
        let work = store.create_list("Work", DEFAULT_PROFILE).unwrap();
        let home = store.create_list("Home", DEFAULT_PROFILE).unwrap();
        let focus = store.create_profile("Focus", &[work]).unwrap();
        let both = store.create_profile("Both", &[work, home]).unwrap();
        let id = store.insert(&NoteRecord { list: work, ..note() }).unwrap();
        let lists_of = |p: i64| store.profiles().unwrap().into_iter().find(|x| x.id == p).map(|x| x.lists);
        store.delete_list(work, true).unwrap();
        // The note is in the trash, and comes back into the default list.
        assert!(store.load().unwrap().iter().all(|n| n.id != id));
        assert_eq!(store.restore(id).unwrap().map(|n| n.list), Some(DEFAULT_LIST));
        // A profile left with nothing shows the default list; one with other lists keeps them.
        assert_eq!(lists_of(focus), Some(vec![DEFAULT_LIST]));
        assert_eq!(lists_of(both), Some(vec![home]));
    }

    #[test]
    fn a_profile_always_shows_a_list() {
        let store = Store::in_memory().unwrap();
        let work = store.create_list("Work", DEFAULT_PROFILE).unwrap();
        let focus = store.create_profile("Focus", &[work]).unwrap();
        let lists_of = |p: i64| store.profiles().unwrap().into_iter().find(|x| x.id == p).map(|x| x.lists);
        // Hiding the last list shows the default one instead.
        store.set_profile_list(focus, work, false).unwrap();
        assert_eq!(lists_of(focus), Some(vec![DEFAULT_LIST]));
        // ...which can't be hidden while it's the only one.
        store.set_profile_list(focus, DEFAULT_LIST, false).unwrap();
        assert_eq!(lists_of(focus), Some(vec![DEFAULT_LIST]));
        store.set_profile_list(focus, work, true).unwrap();
        store.set_profile_list(focus, DEFAULT_LIST, false).unwrap();
        assert_eq!(lists_of(focus), Some(vec![work]));
    }

    #[test]
    fn toggling_lists_and_protected_defaults() {
        let store = Store::in_memory().unwrap();
        let work = store.create_list("Work", DEFAULT_PROFILE).unwrap();
        let focus = store.create_profile("Focus", &[work]).unwrap();
        let id = store.insert(&NoteRecord { list: work, ..note() }).unwrap();
        let lists_of = |p: i64| store.profiles().unwrap().into_iter().find(|x| x.id == p).map(|x| x.lists);
        assert_eq!(lists_of(focus), Some(vec![work]));
        // The catch-all profile shows every list.
        assert!(lists_of(DEFAULT_PROFILE).unwrap().contains(&work));
        store.set_profile_list(focus, DEFAULT_LIST, true).unwrap();
        assert_eq!(lists_of(focus).unwrap().len(), 2);
        store.set_profile_list(focus, DEFAULT_LIST, false).unwrap();
        assert_eq!(lists_of(focus), Some(vec![work]));
        // Deleting a list moves its notes to the default list.
        store.delete_list(work, false).unwrap();
        assert_eq!(store.load().unwrap().iter().find(|n| n.id == id).map(|n| n.list), Some(DEFAULT_LIST));
        // The defaults can't be deleted.
        store.delete_list(DEFAULT_LIST, true).unwrap();
        store.delete_profile(DEFAULT_PROFILE).unwrap();
        assert!(store.lists().unwrap().iter().any(|l| l.id == DEFAULT_LIST));
        assert!(lists_of(DEFAULT_PROFILE).is_some());
        store.delete_profile(focus).unwrap();
        assert_eq!(lists_of(focus), None);
    }

    #[test]
    fn file_store_persists_notes_and_settings() {
        let path = std::env::temp_dir().join(format!("stickit-test-{}.db", std::process::id()));
        let cleanup = || {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
            }
        };
        cleanup();
        {
            let store = Store::open(&path).unwrap();
            store.insert(&note()).unwrap();
            store.set_setting("dock_side", "left");
            store.set_setting("dock_side", "right");
        }
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.load().unwrap().len(), 1);
            assert_eq!(store.setting("dock_side").as_deref(), Some("right"));
            assert_eq!(store.setting("missing"), None);
        }
        cleanup();
    }
}
