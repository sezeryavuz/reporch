//! Briefs — the only channel repos use to talk to each other (§6.5).
//!
//! A brief is content. A ping is attention. Anyone may write a brief; delivering a ping is a
//! privilege (§6). This module holds the content half; `protocol.rs` decides the attention half.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Brief IDs are zero-padded to four digits so a directory listing sorts (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BriefId(u32);

impl BriefId {
    pub const fn new(n: u32) -> Self {
        Self(n)
    }
    pub const fn number(self) -> u32 {
        self.0
    }
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for BriefId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid brief id `{0}`; expected four digits, e.g. 0003")]
pub struct ParseBriefIdError(pub String);

impl FromStr for BriefId {
    type Err = ParseBriefIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseBriefIdError(s.to_string()));
        }
        t.parse::<u32>()
            .map(BriefId)
            .map_err(|_| ParseBriefIdError(s.to_string()))
    }
}

impl TryFrom<String> for BriefId {
    type Error = ParseBriefIdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<BriefId> for String {
    fn from(id: BriefId) -> String {
        id.to_string()
    }
}

/// What a brief is for (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BriefKind {
    Request,
    Reply,
    Fyi,
    Blocker,
    Question,
}

impl BriefKind {
    pub const ALL: [BriefKind; 5] = [
        BriefKind::Request,
        BriefKind::Reply,
        BriefKind::Fyi,
        BriefKind::Blocker,
        BriefKind::Question,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            BriefKind::Request => "request",
            BriefKind::Reply => "reply",
            BriefKind::Fyi => "fyi",
            BriefKind::Blocker => "blocker",
            BriefKind::Question => "question",
        }
    }

    /// Whether this kind is asking the recipient to *do* something. `fyi` is not, and so never
    /// justifies a control-plane nudge on its own.
    pub const fn is_actionable(self) -> bool {
        matches!(
            self,
            BriefKind::Request | BriefKind::Blocker | BriefKind::Question
        )
    }
}

impl fmt::Display for BriefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown brief kind `{0}`; valid kinds are request, reply, fyi, blocker, question")]
pub struct ParseBriefKindError(pub String);

impl FromStr for BriefKind {
    type Err = ParseBriefKindError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "request" => Ok(BriefKind::Request),
            "reply" => Ok(BriefKind::Reply),
            "fyi" => Ok(BriefKind::Fyi),
            "blocker" => Ok(BriefKind::Blocker),
            "question" => Ok(BriefKind::Question),
            o => Err(ParseBriefKindError(o.to_string())),
        }
    }
}

/// Brief lifecycle (§6.5). Written by the CLI, never by hand-editing the file, so that
/// `briefs/index.json` and the files can never disagree (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BriefStatus {
    Unread,
    Read,
    Acked,
    Done,
    Blocked,
    Rejected,
}

impl BriefStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            BriefStatus::Unread => "unread",
            BriefStatus::Read => "read",
            BriefStatus::Acked => "acked",
            BriefStatus::Done => "done",
            BriefStatus::Blocked => "blocked",
            BriefStatus::Rejected => "rejected",
        }
    }

    /// A brief still awaiting attention. Drives the control-plane nudge (§6.3) and the run
    /// screen's BRIEFS column.
    pub const fn is_outstanding(self) -> bool {
        matches!(
            self,
            BriefStatus::Unread | BriefStatus::Read | BriefStatus::Acked
        )
    }

    /// A brief the recipient has finished with, one way or another.
    pub const fn is_closed(self) -> bool {
        matches!(
            self,
            BriefStatus::Done | BriefStatus::Blocked | BriefStatus::Rejected
        )
    }

    /// Whether `self -> next` is a legal transition.
    ///
    /// Statuses only ever move forward. A `done` brief cannot go back to `unread`, because the
    /// index is an audit log and rewriting history in it would make a ping storm unprovable.
    pub const fn can_transition_to(self, next: BriefStatus) -> bool {
        use BriefStatus::*;
        match (self, next) {
            (Unread, Read | Done | Blocked | Rejected | Acked) => true,
            (Read, Acked | Done | Blocked | Rejected) => true,
            (Acked, Done | Blocked | Rejected) => true,
            // Terminal.
            (Done | Blocked | Rejected, _) => false,
            _ => false,
        }
    }
}

impl fmt::Display for BriefStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown brief status `{0}`; valid values are done, blocked, rejected")]
pub struct ParseAckStatusError(pub String);

/// The subset of statuses a session may set via `reporch brief ack` (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStatus {
    Done,
    Blocked,
    Rejected,
}

impl AckStatus {
    pub const fn as_status(self) -> BriefStatus {
        match self {
            AckStatus::Done => BriefStatus::Done,
            AckStatus::Blocked => BriefStatus::Blocked,
            AckStatus::Rejected => BriefStatus::Rejected,
        }
    }
}

impl FromStr for AckStatus {
    type Err = ParseAckStatusError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "done" => Ok(AckStatus::Done),
            "blocked" => Ok(AckStatus::Blocked),
            "rejected" => Ok(AckStatus::Rejected),
            o => Err(ParseAckStatusError(o.to_string())),
        }
    }
}

/// A brief's YAML frontmatter (§6.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefHeader {
    pub id: BriefId,
    pub task: String,
    pub from: String,
    pub to: String,
    pub kind: BriefKind,
    pub subject: String,
    #[serde(default)]
    pub in_reply_to: Option<BriefId>,
    pub created_at: String,
    pub status: BriefStatus,
}

/// A brief: frontmatter plus a markdown body.
#[derive(Debug, Clone, PartialEq)]
pub struct Brief {
    pub header: BriefHeader,
    pub body: String,
}

impl Brief {
    /// The on-disk filename (§6.5): `0003-modulex.to.modulex-ui.md`.
    ///
    /// Zero-padded, sortable, and self-describing in a directory listing.
    pub fn filename(&self) -> String {
        brief_filename(self.header.id, &self.header.from, &self.header.to)
    }
}

/// `0003-modulex.to.modulex-ui.md` (§6.5).
pub fn brief_filename(id: BriefId, from: &str, to: &str) -> String {
    format!("{id}-{from}.to.{to}.md")
}

/// One entry in `briefs/index.json` (§8.3). Briefs and pings share the log deliberately: a ping
/// storm and a brief storm are the same class of failure and belong on one timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IndexEntry {
    Brief(BriefEntry),
    Ping(PingEntry),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefEntry {
    pub id: BriefId,
    pub from: String,
    pub to: String,
    pub kind: BriefKind,
    pub subject: String,
    pub created_at: String,
    pub status: BriefStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<BriefId>,
    pub file: String,
}

/// Who delivered a ping. Every ping is logged with its trigger, so ping storms are visible after
/// the fact rather than inferred (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PingBy {
    Master,
    ControlPlane,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PingEntry {
    /// Always the literal `"ping"`, so the untagged enum can discriminate.
    pub kind: PingKindTag,
    pub to: String,
    pub by: PingBy,
    pub reason: String,
    pub at: String,
}

/// A single-variant tag whose only job is to make `IndexEntry`'s untagged deserialisation
/// unambiguous: a brief's `kind` can never be `"ping"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PingKindTag {
    Ping,
}

/// `briefs/index.json` (§8.3). Append-only: the audit log of the entire task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefIndex {
    pub schema_version: u32,
    pub next_id: u32,
    pub entries: Vec<IndexEntry>,
}

pub const BRIEF_INDEX_SCHEMA_VERSION: u32 = 1;

impl Default for BriefIndex {
    fn default() -> Self {
        Self {
            schema_version: BRIEF_INDEX_SCHEMA_VERSION,
            next_id: 1,
            entries: Vec::new(),
        }
    }
}

impl BriefIndex {
    pub fn briefs(&self) -> impl Iterator<Item = &BriefEntry> {
        self.entries.iter().filter_map(|e| match e {
            IndexEntry::Brief(b) => Some(b),
            IndexEntry::Ping(_) => None,
        })
    }

    pub fn pings(&self) -> impl Iterator<Item = &PingEntry> {
        self.entries.iter().filter_map(|e| match e {
            IndexEntry::Ping(p) => Some(p),
            IndexEntry::Brief(_) => None,
        })
    }

    pub fn brief(&self, id: BriefId) -> Option<&BriefEntry> {
        self.briefs().find(|b| b.id == id)
    }

    /// Briefs addressed to `repo` that are still outstanding.
    pub fn inbox<'a>(&'a self, repo: &'a str) -> impl Iterator<Item = &'a BriefEntry> + 'a {
        self.briefs().filter(move |b| b.to == repo)
    }

    pub fn unread_for(&self, repo: &str) -> usize {
        self.briefs()
            .filter(|b| b.to == repo && b.status == BriefStatus::Unread)
            .count()
    }

    /// Briefs `repo` sent that the recipient has not closed out. Feeds the third §6.3 nudge
    /// condition ("waiting on N repos").
    pub fn open_outbound(&self, repo: &str) -> usize {
        self.briefs()
            .filter(|b| b.from == repo && !b.status.is_closed())
            .count()
    }

    /// Allocate the next brief ID. Callers must hold the advisory lock on the index file
    /// (§7.2) — this function is pure and cannot enforce that.
    pub fn allocate_id(&mut self) -> BriefId {
        let id = BriefId::new(self.next_id);
        self.next_id += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_id_is_zero_padded() {
        assert_eq!(BriefId::new(3).to_string(), "0003");
        assert_eq!(BriefId::new(1234).to_string(), "1234");
        assert_eq!("0003".parse::<BriefId>().unwrap(), BriefId::new(3));
        assert_eq!("3".parse::<BriefId>().unwrap(), BriefId::new(3));
    }

    #[test]
    fn filename_matches_the_spec_example() {
        assert_eq!(
            brief_filename(BriefId::new(3), "modulex", "modulex-ui"),
            "0003-modulex.to.modulex-ui.md"
        );
    }

    #[test]
    fn fyi_is_not_actionable() {
        assert!(!BriefKind::Fyi.is_actionable());
        assert!(!BriefKind::Reply.is_actionable());
        assert!(BriefKind::Request.is_actionable());
        assert!(BriefKind::Blocker.is_actionable());
    }

    #[test]
    fn status_transitions_only_move_forward() {
        use BriefStatus::*;
        assert!(Unread.can_transition_to(Read));
        assert!(Read.can_transition_to(Done));
        assert!(
            Unread.can_transition_to(Done),
            "ack without an explicit read is legal"
        );
        assert!(!Done.can_transition_to(Unread));
        assert!(!Done.can_transition_to(Read));
        assert!(!Rejected.can_transition_to(Done));
        assert!(!Read.can_transition_to(Unread));
    }

    #[test]
    fn terminal_statuses_are_closed_and_not_outstanding() {
        for s in [
            BriefStatus::Done,
            BriefStatus::Blocked,
            BriefStatus::Rejected,
        ] {
            assert!(s.is_closed(), "{s}");
            assert!(!s.is_outstanding(), "{s}");
        }
        for s in [BriefStatus::Unread, BriefStatus::Read, BriefStatus::Acked] {
            assert!(!s.is_closed(), "{s}");
            assert!(s.is_outstanding(), "{s}");
        }
    }

    fn entry(id: u32, from: &str, to: &str, status: BriefStatus) -> IndexEntry {
        IndexEntry::Brief(BriefEntry {
            id: BriefId::new(id),
            from: from.into(),
            to: to.into(),
            kind: BriefKind::Request,
            subject: "s".into(),
            created_at: "2026-08-07T21:44:12Z".into(),
            status,
            read_at: None,
            acked_at: None,
            in_reply_to: None,
            file: brief_filename(BriefId::new(id), from, to),
        })
    }

    #[test]
    fn index_counts_inbox_and_outbox() {
        let idx = BriefIndex {
            schema_version: 1,
            next_id: 4,
            entries: vec![
                entry(1, "modulex", "modulex-ui", BriefStatus::Unread),
                entry(2, "modulex", "modulex-js", BriefStatus::Done),
                entry(3, "modulex-ui", "modulex", BriefStatus::Unread),
                IndexEntry::Ping(PingEntry {
                    kind: PingKindTag::Ping,
                    to: "modulex-ui".into(),
                    by: PingBy::Master,
                    reason: "brief 0001 filed".into(),
                    at: "2026-08-07T21:44:13Z".into(),
                }),
            ],
        };
        assert_eq!(idx.unread_for("modulex-ui"), 1);
        assert_eq!(idx.unread_for("modulex"), 1);
        assert_eq!(idx.unread_for("modulex-www"), 0);
        assert_eq!(idx.open_outbound("modulex"), 1, "0002 is done, 0001 is not");
        assert_eq!(idx.briefs().count(), 3);
        assert_eq!(idx.pings().count(), 1);
    }

    /// Briefs and pings share one log, so the untagged enum must not confuse them.
    #[test]
    fn index_entries_round_trip_through_json() {
        let idx = BriefIndex {
            schema_version: 1,
            next_id: 3,
            entries: vec![
                entry(1, "a", "b", BriefStatus::Done),
                IndexEntry::Ping(PingEntry {
                    kind: PingKindTag::Ping,
                    to: "b".into(),
                    by: PingBy::ControlPlane,
                    reason: "unread inbox while idle".into(),
                    at: "2026-08-07T21:59:10Z".into(),
                }),
            ],
        };
        let json = serde_json::to_string(&idx).unwrap();
        let back: BriefIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(idx, back);
        assert_eq!(back.briefs().count(), 1);
        assert_eq!(back.pings().count(), 1);
    }

    #[test]
    fn allocate_id_is_monotonic() {
        let mut idx = BriefIndex::default();
        assert_eq!(idx.allocate_id(), BriefId::new(1));
        assert_eq!(idx.allocate_id(), BriefId::new(2));
        assert_eq!(idx.next_id, 3);
    }
}
