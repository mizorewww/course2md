//! How each place of a library lists its notes: the sort it remembers, the order
//! the person arranged by hand, and when each note was added and last read.
//!
//! The record lives in the library's own folder, beside the folder record, so it
//! travels with the library. Notes are named by their course id, never by where
//! their folder is, so moving, renaming or refiling a note keeps its place. The
//! record is advisory: a missing, unknown or damaged entry falls back to the
//! default sort instead of keeping the library from listing its notes.
use anyhow::{Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{cmp::Ordering, collections::BTreeMap, path::Path};

const FILE: &str = ".course2md-order.json";
const LOCK: &str = ".course2md-order.lock";

/// What a place's notes are sorted by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SortRule {
    /// The order the person arranged.
    Manual,
    /// When the note came into the library.
    #[default]
    Added,
    /// When the video was published.
    Published,
    Title,
    /// The video's length.
    Duration,
    /// When the note was last opened.
    LastRead,
}

impl SortRule {
    pub const ALL: [SortRule; 6] = [
        SortRule::Manual,
        SortRule::Added,
        SortRule::Published,
        SortRule::Title,
        SortRule::Duration,
        SortRule::LastRead,
    ];

    fn code(self) -> &'static str {
        match self {
            SortRule::Manual => "manual",
            SortRule::Added => "added",
            SortRule::Published => "published",
            SortRule::Title => "title",
            SortRule::Duration => "duration",
            SortRule::LastRead => "last_read",
        }
    }

    /// Whether the rule runs both ways; a hand-made order has one.
    pub fn has_direction(self) -> bool {
        self != SortRule::Manual
    }
}

impl Serialize for SortRule {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

/// A rule a newer app wrote reads as the default rather than failing the record.
impl<'de> Deserialize<'de> for SortRule {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        Ok(SortRule::ALL
            .into_iter()
            .find(|rule| rule.code() == code)
            .unwrap_or_default())
    }
}

/// A place's sort: its rule and, for a rule that has one, its direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    #[serde(default)]
    pub rule: SortRule,
    /// Largest first: newest, longest, Z before A, most recently read.
    #[serde(default = "descending_default")]
    pub descending: bool,
}

fn descending_default() -> bool {
    true
}

/// Newest added first.
impl Default for Sort {
    fn default() -> Self {
        Self {
            rule: SortRule::Added,
            descending: true,
        }
    }
}

/// Where a list of notes is shown, each with a sort of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Place {
    /// Every note.
    All,
    /// The library's notes outside any folder.
    Unfiled,
    Folder(u64),
}

impl Place {
    fn key(self) -> String {
        match self {
            Place::All => "all".into(),
            Place::Unfiled => "unfiled".into(),
            Place::Folder(id) => format!("folder:{id}"),
        }
    }
}

/// How many complete scans in a row a note must be missing from before the record
/// forgets it: a disk that answers slowly, or a note being replaced, does not cost
/// it its place.
pub const FORGET_AFTER: u32 = 3;

/// Reading a note again within this long, in milliseconds, is not written down:
/// last read only needs to be roughly right, and opening a note should not cost a
/// disk flush each time.
pub const READ_INTERVAL_MS: u64 = 5 * 60 * 1000;

/// A place's sort as stored: kept as written, so a rule or field a newer app added
/// survives this one saving the record.
fn read_sort(value: Option<&serde_json::Value>) -> Sort {
    value
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// `sort` written over what was stored, keeping fields this app does not know;
/// `None` when nothing but the default would be left.
fn write_sort(stored: Option<serde_json::Value>, sort: Sort) -> Option<serde_json::Value> {
    let mut object = match stored {
        Some(serde_json::Value::Object(object)) => object,
        _ => Default::default(),
    };
    object.insert("rule".into(), serde_json::to_value(sort.rule).ok()?);
    object.insert("descending".into(), sort.descending.into());
    let only_default = sort == Sort::default()
        && object
            .keys()
            .all(|key| key == "rule" || key == "descending");
    (!only_default).then_some(serde_json::Value::Object(object))
}

/// One library's record of how its places list their notes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Order {
    /// Place → its sort; a place without one uses the default.
    #[serde(default)]
    sorts: BTreeMap<String, serde_json::Value>,
    /// Place → note keys in the order the person arranged.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    manual: BTreeMap<String, Vec<String>>,
    /// Note → when it was first seen in the library, in milliseconds since the epoch.
    /// A regenerated note keeps the time it was first added.
    #[serde(default)]
    added: BTreeMap<String, u64>,
    /// Note → when it was last opened, in milliseconds since the epoch.
    #[serde(default)]
    read: BTreeMap<String, u64>,
    /// Note → complete scans in a row it was missing from.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    missing: BTreeMap<String, u32>,
    /// What a newer app wrote, kept as it was.
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl Order {
    /// The library's record, or an empty one when there is none or it cannot be read;
    /// a damaged record falls back to its last good copy.
    pub fn load(root: &Path) -> Self {
        let path = root.join(FILE);
        let parse = |bytes: Vec<u8>| serde_json::from_slice::<Self>(&bytes).ok();
        match desktop_prefs::store::read(&path) {
            Ok(bytes) => parse(bytes)
                .or_else(|| {
                    desktop_prefs::store::read_backup(&path)
                        .ok()
                        .and_then(parse)
                })
                .unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Changes the record under the library's lock, from what is on disk now. The
    /// damaged bytes of an unreadable record are kept aside, not overwritten.
    pub fn edit(root: &Path, change: impl FnOnce(&mut Self)) -> Result<Self> {
        ensure!(
            root.is_dir(),
            c2m_i18n::tr!("笔记库文件夹暂时无法访问，请重新连接磁盘后再试")
        );
        let _lock = c2m_process::runtime::lock_file(&root.join(LOCK))?;
        let mut order = Self::load(root);
        let before = order.clone();
        change(&mut order);
        if order != before {
            desktop_prefs::store::save(&root.join(FILE), &serde_json::to_vec_pretty(&order)?)?;
        }
        Ok(order)
    }

    /// Records what a scan found: a note not seen before is added when it was made.
    /// A complete scan (nothing unread, nothing waiting on the person) counts the
    /// notes it did not find; one missing from [`FORGET_AFTER`] complete scans in a
    /// row is forgotten, with its place in hand-made orders. Writes only when
    /// something changed, and never fails the scan.
    pub fn observe(root: &Path, notes: &[(String, u64)], complete: bool) -> Self {
        let mut order = Self::load(root);
        if order.note_seen(notes, complete) {
            return Self::edit(root, |order| {
                order.note_seen(notes, complete);
            })
            .unwrap_or(order);
        }
        order
    }

    fn note_seen(&mut self, notes: &[(String, u64)], complete: bool) -> bool {
        let before = self.clone();
        let present: std::collections::HashSet<&str> =
            notes.iter().map(|(key, _)| key.as_str()).collect();
        for (key, created) in notes {
            self.added.entry(key.clone()).or_insert(*created);
            self.missing.remove(key);
        }
        if complete {
            let known: std::collections::BTreeSet<String> = self
                .added
                .keys()
                .chain(self.read.keys())
                .filter(|key| !present.contains(key.as_str()))
                .cloned()
                .collect();
            let mut forgotten = std::collections::HashSet::new();
            for key in known {
                let count = self.missing.entry(key.clone()).or_insert(0);
                if *count < FORGET_AFTER {
                    *count += 1;
                }
                if *count >= FORGET_AFTER {
                    forgotten.insert(key);
                }
            }
            for key in &forgotten {
                self.added.remove(key);
                self.read.remove(key);
                self.missing.remove(key);
            }
            for keys in self.manual.values_mut() {
                keys.retain(|key| !forgotten.contains(key));
            }
            self.manual.retain(|_, keys| !keys.is_empty());
            self.missing
                .retain(|key, _| self.added.contains_key(key) || self.read.contains_key(key));
        }
        *self != before
    }

    pub fn sort(&self, place: Place) -> Sort {
        read_sort(self.sorts.get(&place.key()))
    }

    /// The place's hand-made order, as note keys.
    pub fn manual(&self, place: Place) -> &[String] {
        self.manual
            .get(&place.key())
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Sets the place's sort and, when given, its hand-made order; an empty order
    /// is removed rather than stored.
    pub fn set_place(&mut self, place: Place, sort: Sort, manual: Option<Vec<String>>) {
        let key = place.key();
        if let Some(value) = write_sort(self.sorts.remove(&key), sort) {
            self.sorts.insert(key.clone(), value);
        }
        match manual {
            Some(manual) if manual.is_empty() => {
                self.manual.remove(&key);
            }
            Some(manual) => {
                self.manual.insert(key, manual);
            }
            None => {}
        }
    }

    /// Forgets a removed folder's sort and order.
    pub fn forget_folder(&mut self, id: u64) {
        let key = Place::Folder(id).key();
        self.sorts.remove(&key);
        self.manual.remove(&key);
    }

    pub fn added(&self, key: &str) -> Option<u64> {
        self.added.get(key).copied()
    }

    /// Whether the record knows the note: it was seen and not yet forgotten.
    pub fn knows(&self, key: &str) -> bool {
        self.added.contains_key(key)
    }

    pub fn last_read(&self, key: &str) -> Option<u64> {
        self.read.get(key).copied()
    }

    /// Notes that the note was opened at `at`, unless it was within
    /// [`READ_INTERVAL_MS`] of the time already noted. Returns whether it changed.
    pub fn mark_read(&mut self, key: &str, at: u64) -> bool {
        if self
            .read
            .get(key)
            .is_some_and(|before| at.saturating_sub(*before) < READ_INTERVAL_MS)
        {
            return false;
        }
        self.read.insert(key.to_owned(), at);
        true
    }
}

/// The sort and hand-made order of every note, across libraries, as the app keeps
/// it in its own record: notes are `<library id>/<note key>`, so it does not depend
/// on which library is the default or reachable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaceOrder {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sort: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    manual: Vec<String>,
    /// What a newer app wrote, kept as it was.
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl PlaceOrder {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn sort(&self) -> Sort {
        read_sort(self.sort.as_ref())
    }

    pub fn manual(&self) -> &[String] {
        &self.manual
    }

    /// Sets the sort and, when given, the hand-made order.
    pub fn set(&mut self, sort: Sort, manual: Option<Vec<String>>) {
        self.sort = write_sort(self.sort.take(), sort);
        if let Some(manual) = manual {
            self.manual = manual;
        }
    }
}

/// Each note's key in its library's record, in the order given: its course id, or,
/// for a copy of a note (another folder with the same course id), the id with the
/// copy's folder, so moving one copy never moves the other. Of the notes sharing an
/// id, the one whose folder sorts first keeps the plain id.
pub fn note_keys(notes: &[(&Path, &str)]) -> Vec<String> {
    let mut first: std::collections::HashMap<&str, &Path> = Default::default();
    for (folder, id) in notes {
        let entry = first.entry(id).or_insert(folder);
        if folder < entry {
            *entry = folder;
        }
    }
    notes
        .iter()
        .map(|(folder, id)| {
            if first.get(id) == Some(folder) {
                (*id).to_owned()
            } else {
                let folder = folder
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                format!("{id}@{folder}")
            }
        })
        .collect()
}

/// What a rule sorts one note by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteFacts {
    /// The note's key in the record: its course id.
    pub key: String,
    pub title: String,
    /// When it was added, in milliseconds since the epoch.
    pub added: u64,
    /// When its video was published, as `YYYY-MM-DD`.
    pub published: Option<String>,
    /// The video's length in seconds.
    pub duration: Option<f64>,
    /// When it was last opened, in milliseconds since the epoch.
    pub read: Option<u64>,
}

/// Titles compare without case, and runs of digits by their value, so
/// “第 2 集” comes before “第 10 集”.
pub fn compare_titles(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let run = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(digit) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(digit);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (run(&mut a), run(&mut b));
                let (tx, ty) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// The ties of every rule: by title, then by key, so a list never reshuffles.
fn by_title(a: &NoteFacts, b: &NoteFacts) -> Ordering {
    compare_titles(&a.title, &b.title)
        .then_with(|| a.title.cmp(&b.title))
        .then_with(|| a.key.cmp(&b.key))
}

/// A value some notes lack: those come last whichever way the rule runs.
fn by_value<T: PartialOrd>(a: Option<T>, b: Option<T>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            let order = a.partial_cmp(&b).unwrap_or(Ordering::Equal);
            if descending { order.reverse() } else { order }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The notes' positions in the place's order. A hand-made order lists the notes it
/// names first; notes it does not name yet, such as new ones, follow in the order
/// they were added.
pub fn arrange(notes: &[NoteFacts], sort: Sort, manual: &[String]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..notes.len()).collect();
    let descending = sort.descending;
    match sort.rule {
        SortRule::Manual => {
            let mut position: std::collections::HashMap<&str, usize> = Default::default();
            for (index, key) in manual.iter().enumerate() {
                position.entry(key.as_str()).or_insert(index);
            }
            indices.sort_by(|&a, &b| {
                let (a, b) = (&notes[a], &notes[b]);
                by_value(
                    position.get(a.key.as_str()),
                    position.get(b.key.as_str()),
                    false,
                )
                .then_with(|| a.added.cmp(&b.added))
                .then_with(|| by_title(a, b))
            });
        }
        rule => indices.sort_by(|&a, &b| {
            let (a, b) = (&notes[a], &notes[b]);
            let order = match rule {
                SortRule::Added => by_value(Some(a.added), Some(b.added), descending),
                SortRule::Published => {
                    by_value(a.published.as_ref(), b.published.as_ref(), descending)
                }
                SortRule::Duration => by_value(a.duration, b.duration, descending),
                SortRule::LastRead => by_value(a.read, b.read, descending),
                SortRule::Title | SortRule::Manual => {
                    let order = by_title(a, b);
                    if descending { order.reverse() } else { order }
                }
            };
            order.then_with(|| by_title(a, b))
        }),
    }
    indices
}

/// `keys` with `moving` taken to where `target` is: after it when it came from
/// before it, otherwise before it. `None` when either is not in the list.
pub fn move_onto(keys: &[String], moving: &str, target: &str) -> Option<Vec<String>> {
    let from = keys.iter().position(|key| key == moving)?;
    let to = keys.iter().position(|key| key == target)?;
    let mut moved = keys.to_vec();
    let key = moved.remove(from);
    moved.insert(to, key);
    Some(moved)
}

/// `keys` with `moving` `step` places later (earlier when negative), and its new
/// position. `None` when it cannot move that way.
pub fn move_by(keys: &[String], moving: &str, step: isize) -> Option<(Vec<String>, usize)> {
    let from = keys.iter().position(|key| key == moving)?;
    let to = from
        .checked_add_signed(step)
        .filter(|to| *to < keys.len() && *to != from)?;
    let mut moved = keys.to_vec();
    let key = moved.remove(from);
    moved.insert(to, key);
    Some((moved, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(key: &str, title: &str, added: u64) -> NoteFacts {
        NoteFacts {
            key: key.into(),
            title: title.into(),
            added,
            ..Default::default()
        }
    }

    fn keys(notes: &[NoteFacts], order: &[usize]) -> Vec<String> {
        order
            .iter()
            .map(|index| notes[*index].key.clone())
            .collect()
    }

    fn sort(rule: SortRule, descending: bool) -> Sort {
        Sort { rule, descending }
    }

    #[test]
    fn the_default_is_newest_added_first() {
        let notes = [note("a", "A", 1), note("b", "B", 3), note("c", "C", 2)];
        let order = arrange(&notes, Sort::default(), &[]);
        assert_eq!(keys(&notes, &order), ["b", "c", "a"]);
        let order = arrange(&notes, sort(SortRule::Added, false), &[]);
        assert_eq!(keys(&notes, &order), ["a", "c", "b"]);
    }

    #[test]
    fn titles_sort_naturally_and_ties_fall_back_to_title() {
        let notes = [
            note("x", "第 10 集", 1),
            note("y", "第 2 集", 1),
            note("z", "episode 1", 1),
            note("w", "Episode 01", 1),
        ];
        let order = arrange(&notes, sort(SortRule::Title, false), &[]);
        assert_eq!(keys(&notes, &order), ["w", "z", "y", "x"]);
        let order = arrange(&notes, sort(SortRule::Title, true), &[]);
        assert_eq!(keys(&notes, &order), ["x", "y", "z", "w"]);
        // Equal times list by title, whichever way the rule runs.
        for descending in [false, true] {
            let order = arrange(&notes, sort(SortRule::Added, descending), &[]);
            assert_eq!(keys(&notes, &order), ["w", "z", "y", "x"]);
        }
        assert_eq!(compare_titles("a9", "a10"), Ordering::Less);
        assert_eq!(compare_titles("abc", "ABD"), Ordering::Less);
    }

    #[test]
    fn notes_without_a_value_come_last_either_way() {
        let mut short = note("short", "B", 1);
        short.duration = Some(60.);
        short.published = Some("2024-03-01".into());
        short.read = Some(10);
        let mut long = note("long", "C", 2);
        long.duration = Some(3600.);
        long.published = Some("2025-01-09".into());
        long.read = Some(20);
        let unknown = note("unknown", "A", 3);
        let notes = [unknown, short, long];
        for (rule, ascending) in [
            (SortRule::Duration, ["short", "long", "unknown"]),
            (SortRule::Published, ["short", "long", "unknown"]),
            (SortRule::LastRead, ["short", "long", "unknown"]),
        ] {
            let order = arrange(&notes, sort(rule, false), &[]);
            assert_eq!(keys(&notes, &order), ascending, "{rule:?}");
            let order = arrange(&notes, sort(rule, true), &[]);
            assert_eq!(
                keys(&notes, &order),
                [ascending[1], ascending[0], "unknown"],
                "{rule:?}"
            );
        }
    }

    #[test]
    fn a_manual_order_puts_new_notes_last_in_the_order_they_came() {
        let notes = [
            note("a", "A", 1),
            note("b", "B", 2),
            note("c", "C", 3),
            note("new2", "New 2", 9),
            note("new1", "New 1", 8),
        ];
        let manual: Vec<String> = ["c", "gone", "a", "b", "a"].map(String::from).into();
        let order = arrange(&notes, sort(SortRule::Manual, true), &manual);
        assert_eq!(keys(&notes, &order), ["c", "a", "b", "new1", "new2"]);
    }

    #[test]
    fn dragging_and_stepping_move_one_note() {
        let list: Vec<String> = ["a", "b", "c", "d"].map(String::from).into();
        // Down onto a later note lands after it; up onto an earlier one, before it.
        assert_eq!(move_onto(&list, "a", "c").unwrap(), ["b", "c", "a", "d"]);
        assert_eq!(move_onto(&list, "d", "b").unwrap(), ["a", "d", "b", "c"]);
        assert_eq!(move_onto(&list, "b", "b").unwrap(), list);
        assert!(move_onto(&list, "x", "b").is_none());
        assert_eq!(
            move_by(&list, "b", 1).unwrap(),
            (vec!["a".into(), "c".into(), "b".into(), "d".into()], 2)
        );
        assert_eq!(move_by(&list, "c", -2).unwrap().1, 0);
        assert!(move_by(&list, "a", -1).is_none());
        assert!(move_by(&list, "d", 1).is_none());
    }

    #[test]
    fn dragging_under_a_rule_starts_the_manual_order_from_what_is_shown() {
        let notes = [note("a", "A", 1), note("b", "B", 2), note("c", "C", 3)];
        let shown = keys(&notes, &arrange(&notes, sort(SortRule::Title, true), &[]));
        assert_eq!(shown, ["c", "b", "a"]);
        let mut order = Order::default();
        order.set_place(
            Place::Folder(4),
            sort(SortRule::Manual, true),
            move_onto(&shown, "a", "c"),
        );
        let place = Place::Folder(4);
        let arranged = arrange(&notes, order.sort(place), order.manual(place));
        assert_eq!(keys(&notes, &arranged), ["a", "c", "b"]);
        // Other places keep their own sort.
        assert_eq!(order.sort(Place::All), Sort::default());
        assert_eq!(order.sort(Place::Unfiled), Sort::default());
    }

    #[test]
    fn the_record_survives_a_restart_moves_and_a_library_move() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        std::fs::create_dir(&root).unwrap();
        let seen = [("course-a".to_owned(), 100), ("course-b".to_owned(), 200)];
        let observed = Order::observe(&root, &seen, true);
        assert_eq!(observed.added("course-a"), Some(100));
        Order::edit(&root, |order| {
            order.set_place(
                Place::Folder(1),
                sort(SortRule::Manual, true),
                Some(vec!["course-b".into(), "course-a".into()]),
            );
            order.set_place(Place::All, sort(SortRule::Title, false), None);
            order.mark_read("course-a", 500);
        })
        .unwrap();
        // A regenerated note keeps when it was added.
        let again = Order::observe(
            &root,
            &[("course-a".into(), 900), ("course-b".into(), 200)],
            true,
        );
        assert_eq!(again.added("course-a"), Some(100));
        assert_eq!(again.last_read("course-a"), Some(500));
        // The record lives in the library, so a moved library keeps it.
        let moved = temp.path().join("moved");
        std::fs::create_dir(&moved).unwrap();
        for entry in std::fs::read_dir(&root).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), moved.join(entry.file_name())).unwrap();
        }
        let loaded = Order::load(&moved);
        assert_eq!(loaded, again);
        assert_eq!(loaded.manual(Place::Folder(1)), ["course-b", "course-a"]);
        assert_eq!(loaded.sort(Place::All), sort(SortRule::Title, false));
        // A scan that is not complete never counts a note as gone.
        for _ in 0..FORGET_AFTER + 1 {
            let partial = Order::observe(&moved, &[("course-b".into(), 200)], false);
            assert_eq!(partial.added("course-a"), Some(100));
        }
        // Missing from one complete scan, or two, a note keeps its place; a scan
        // that finds it again starts the count over.
        for _ in 1..FORGET_AFTER {
            let missed = Order::observe(&moved, &[("course-b".into(), 200)], true);
            assert_eq!(missed.added("course-a"), Some(100));
            assert_eq!(missed.manual(Place::Folder(1)), ["course-b", "course-a"]);
        }
        Order::observe(&moved, &seen, true);
        for _ in 1..FORGET_AFTER {
            Order::observe(&moved, &[("course-b".into(), 200)], true);
        }
        assert_eq!(Order::load(&moved).last_read("course-a"), Some(500));
        // Missing from enough complete scans in a row, it is forgotten everywhere.
        let gone = Order::observe(&moved, &[("course-b".into(), 200)], true);
        assert_eq!(gone.added("course-a"), None);
        assert_eq!(gone.last_read("course-a"), None);
        assert_eq!(gone.manual(Place::Folder(1)), ["course-b"]);
        assert_eq!(gone, Order::load(&moved));
    }

    #[test]
    fn what_a_newer_app_wrote_survives_a_save() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(
            root.join(FILE),
            r#"{"sorts":{"all":{"rule":"popularity","descending":false,"group":"x"},
                "unfiled":{"rule":"title","pin":true}},
               "future":{"a":1},"added":{"n":5}}"#,
        )
        .unwrap();
        let order = Order::edit(root, |order| {
            order.set_place(Place::Folder(3), sort(SortRule::Duration, true), None);
            order.set_place(Place::Unfiled, sort(SortRule::Title, false), None);
        })
        .unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join(FILE)).unwrap()).unwrap();
        // The unknown rule and fields stay as written; the place this app changed
        // keeps its unknown field too.
        assert_eq!(written["sorts"]["all"]["rule"], "popularity");
        assert_eq!(written["sorts"]["all"]["group"], "x");
        assert_eq!(written["future"]["a"], 1);
        assert_eq!(written["sorts"]["unfiled"]["pin"], true);
        assert_eq!(written["sorts"]["unfiled"]["rule"], "title");
        assert_eq!(written["sorts"]["unfiled"]["descending"], false);
        assert_eq!(written["sorts"]["folder:3"]["rule"], "duration");
        assert_eq!(order.sort(Place::All), sort(SortRule::Added, false));
        assert_eq!(order.added("n"), Some(5));
        // An unchanged sort writes no manual order, and an empty one is not stored.
        assert!(written.get("manual").is_none());
        let mut order = order;
        order.set_place(
            Place::Unfiled,
            sort(SortRule::Manual, true),
            Some(Vec::new()),
        );
        assert!(order.manual.is_empty());
    }

    #[test]
    fn reading_again_soon_is_not_written_down() {
        let mut order = Order::default();
        assert!(order.mark_read("n", 1_000));
        assert!(!order.mark_read("n", 1_000 + READ_INTERVAL_MS - 1));
        assert_eq!(order.last_read("n"), Some(1_000));
        assert!(order.mark_read("n", 1_000 + READ_INTERVAL_MS));
        assert_eq!(order.last_read("n"), Some(1_000 + READ_INTERVAL_MS));
    }

    #[test]
    fn copies_of_a_note_get_keys_of_their_own() {
        let notes = [
            (Path::new("b/course-x"), "course-x"),
            (Path::new("course-y"), "course-y"),
            (Path::new("a/course-x"), "course-x"),
        ];
        let keys = note_keys(&notes);
        assert_eq!(keys, ["course-x@b/course-x", "course-y", "course-x"]);
        // Moving one copy leaves the other where it was.
        let notes = [
            note("course-x", "X", 1),
            note("course-x@b/course-x", "X", 1),
            note("course-y", "Y", 2),
        ];
        let list: Vec<String> = notes.iter().map(|n| n.key.clone()).collect();
        let moved = move_onto(&list, "course-x", "course-y").unwrap();
        let arranged = arrange(&notes, sort(SortRule::Manual, true), &moved);
        assert_eq!(
            keys_of(&notes, &arranged),
            ["course-x@b/course-x", "course-y", "course-x"]
        );
    }

    fn keys_of(notes: &[NoteFacts], order: &[usize]) -> Vec<String> {
        keys(notes, order)
    }

    #[test]
    fn every_notes_order_keeps_unknown_fields_and_sets_its_sort() {
        let mut order: PlaceOrder = serde_json::from_str(
            r#"{"sort":{"rule":"title","descending":false},"manual":["l/a"],"later":2}"#,
        )
        .unwrap();
        assert_eq!(order.sort(), sort(SortRule::Title, false));
        order.set(sort(SortRule::Manual, false), None);
        assert_eq!(order.manual(), ["l/a"]);
        let written = serde_json::to_value(&order).unwrap();
        assert_eq!(written["later"], 2);
        assert_eq!(written["sort"]["rule"], "manual");
        assert!(PlaceOrder::default().is_empty());
    }

    #[test]
    fn a_missing_damaged_or_newer_record_falls_back_without_losing_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert_eq!(Order::load(root), Order::default());
        assert_eq!(Order::load(&root.join("disconnected")), Order::default());
        assert!(Order::edit(&root.join("disconnected"), |_| {}).is_err());
        // Entries and rules a newer app wrote are read as far as they are understood.
        std::fs::write(
            root.join(FILE),
            r#"{"sorts":{"all":{"rule":"popularity","descending":false},"unfiled":{"rule":"title"}},"future":1}"#,
        )
        .unwrap();
        let newer = Order::load(root);
        assert_eq!(newer.sort(Place::All), sort(SortRule::Added, false));
        assert_eq!(newer.sort(Place::Unfiled), sort(SortRule::Title, true));
        Order::edit(root, |order| {
            order.set_place(Place::Folder(2), sort(SortRule::Duration, false), None)
        })
        .unwrap();
        // A damaged record reads its last good copy, and a save keeps the damaged bytes.
        std::fs::write(root.join(FILE), "damaged").unwrap();
        let restored = Order::load(root);
        assert_eq!(restored.sort(Place::Unfiled), sort(SortRule::Title, true));
        Order::edit(root, |order| {
            order.mark_read("note", 1);
        })
        .unwrap();
        let kept = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains("damaged"))
            .count();
        assert_eq!(kept, 1);
        assert_eq!(Order::load(root).last_read("note"), Some(1));
    }
}
