//! The `new project` and `new task` flows (§5.2–§5.7, §12.1).
//!
//! Held as an explicit state machine rather than nested callbacks, because ratatui is immediate
//! mode: every frame redraws from this value, so the flow *is* the state
//! (`docs/ratatui-notes.md` §1).

use std::collections::BTreeSet;

use reporch_core::{DepGraph, Effort, EffortPlan, ProjectBase};
use reporch_scan::{FoundRepo, Ranked, Selection};

/// Where the user is in a flow.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    // ---- new project (§5.2–§5.4) ----
    /// One prompt: the project name. It is the primary ranking signal for the next step, so it
    /// is taken before scanning (§5.2).
    ProjectName,
    /// Scanning for repositories.
    Scanning,
    /// The §5.3 multi-select, with live re-ranking.
    PickRepos,
    /// The §12.1 consent screen — once per project, actively confirmed, never a default.
    Consent,
    /// Parallel per-repo analysis with real progress (§5.4).
    Analysing,

    // ---- new task (§5.6–§5.8) ----
    /// A multi-line editor for the brief.
    TaskBrief,
    /// One model, applied to every repo alike (§5.6).
    PickModel,
    /// The master's effort; workers derive from it.
    PickEffort,
    /// The election, shown and **editable** before the task starts (§5.7).
    ConfirmMaster,
    /// The task is running; the run screen is showing.
    Running,

    Done,
    Failed(String),
}

/// Durable Claude aliases offered on the §5.6 screen.
///
/// `opus` remains the predictable default. `best` is the portable way to request Fable 5: Claude
/// falls back to its latest Opus model when the organization does not have Fable access. Direct
/// Fable and long-context aliases are explicit opt-ins because non-interactive Claude sessions
/// cannot display the usage-credit consent dialog.
pub const MODELS: [(&str, &str); 8] = [
    (
        "opus",
        "latest Opus; the predictable default for complex cross-repo work",
    ),
    (
        "best",
        "Fable 5 when available, otherwise latest Opus; access and billing vary",
    ),
    (
        "fable",
        "strict Fable 5; requires organization access and billing may vary",
    ),
    (
        "fable[1m]",
        "Fable with explicit 1M context; requires organization access and usage credits",
    ),
    (
        "opus[1m]",
        "Opus with 1M context; availability and usage credits vary by account",
    ),
    (
        "sonnet",
        "latest Sonnet; faster and cheaper for everyday feature work",
    ),
    (
        "sonnet[1m]",
        "Sonnet with 1M context; availability and usage credits vary by account",
    ),
    ("haiku", "fastest and cheapest; small, well-specified edits"),
];

/// The whole flow's state.
#[derive(Debug)]
pub struct Wizard {
    pub step: Step,
    // new project
    pub project_name: String,
    pub candidates: Vec<FoundRepo>,
    pub ranked: Vec<Ranked>,
    pub selected: BTreeSet<String>,
    pub cursor: usize,
    pub filter: String,
    pub filtering: bool,
    pub scan_secs: f64,
    /// Per-repo analysis progress, for the §5.4 screen: real progress, not a spinner.
    pub analysis: Vec<(String, AnalysisState)>,
    // new task
    pub brief: String,
    pub model_cursor: usize,
    pub effort_cursor: usize,
    pub ultracode: bool,
    pub master: String,
    pub master_reason: String,
    pub master_cursor: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnalysisState {
    Waiting,
    Running { elapsed_secs: u64 },
    Done { one_liner: String },
    Failed(String),
}

impl Default for Wizard {
    fn default() -> Self {
        Self::new()
    }
}

impl Wizard {
    pub fn new() -> Self {
        Self {
            step: Step::ProjectName,
            project_name: String::new(),
            candidates: Vec::new(),
            ranked: Vec::new(),
            selected: BTreeSet::new(),
            cursor: 0,
            filter: String::new(),
            filtering: false,
            scan_secs: 0.0,
            analysis: Vec::new(),
            brief: String::new(),
            model_cursor: 0,
            effort_cursor: 3,
            ultracode: false,
            master: String::new(),
            master_reason: String::new(),
            master_cursor: 0,
            error: None,
        }
    }

    /// Start a `new task` flow for an analysed project.
    pub fn for_task(project: &ProjectBase) -> Self {
        let mut w = Self::new();
        w.project_name = project.name.clone();
        w.model_cursor = MODELS
            .iter()
            .position(|(alias, _)| *alias == project.settings.default_model)
            .unwrap_or(0);
        w.step = Step::TaskBrief;
        w
    }

    // ---- §5.3 multi-select ------------------------------------------------

    /// Re-rank against the current selection.
    ///
    /// **Called on every selection change**, which is what makes §5.3's promise true: picking
    /// `modulex` immediately pulls its remote-org and filesystem siblings up, before the user has
    /// typed anything.
    pub fn rerank(&mut self) {
        let chosen: Vec<&FoundRepo> = self
            .candidates
            .iter()
            .filter(|r| self.selected.contains(&r.name))
            .collect();
        let sel = Selection::from_repos(chosen);
        self.ranked = reporch_scan::rank(&self.candidates, &self.project_name, &sel);
        // Keep the cursor in range after a reorder.
        let n = self.visible().len();
        if n > 0 && self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    /// The rows currently on screen, after filtering.
    pub fn visible(&self) -> Vec<&Ranked> {
        reporch_scan::filter(&self.ranked, &self.filter)
    }

    pub fn toggle_current(&mut self) {
        let Some(r) = self.visible().get(self.cursor).map(|r| r.repo.name.clone()) else {
            return;
        };
        if !self.selected.remove(&r) {
            self.selected.insert(r);
        }
        // The whole point of §5.3: the list reorders the instant you choose something.
        self.rerank();
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let n = self.visible().len();
        if n == 0 {
            return;
        }
        let cur = self.cursor as isize;
        self.cursor = (cur + delta).rem_euclid(n as isize) as usize;
    }

    pub fn selected_repos(&self) -> Vec<FoundRepo> {
        self.candidates
            .iter()
            .filter(|r| self.selected.contains(&r.name))
            .cloned()
            .collect()
    }

    // ---- §5.6 model and effort --------------------------------------------

    pub fn model(&self) -> &'static str {
        MODELS[self.model_cursor.min(MODELS.len() - 1)].0
    }

    pub fn effort(&self) -> Effort {
        Effort::ALL[self.effort_cursor.min(Effort::ALL.len() - 1)]
    }

    /// The plan the §5.6 ladder derives from the single choice the user makes.
    pub fn effort_plan(&self) -> EffortPlan {
        EffortPlan::new(self.effort(), self.ultracode)
    }

    // ---- §5.7 master election ---------------------------------------------

    /// A deterministic election from the dependency graph alone.
    ///
    /// Used when the model's suggestion is unavailable, and as the tie-break within its
    /// `likely_involved` set. Biased toward the repo **furthest upstream**, because contracts
    /// flow downstream and a master waiting on a repo it delegated to will stall (§5.7).
    pub fn elect_from_graph(project: &ProjectBase) -> (String, String) {
        let names = project.repo_names();
        let graph = DepGraph::build(&names, &project.graph);
        let ranked = graph.rank_upstream(&names);
        match ranked.first() {
            Some(top) => {
                let n = graph.downstream_reach(top);
                let reason = if n == 0 {
                    format!(
                        "`{top}` — no contract dependencies were detected between these \
                         repositories, so this is alphabetical. Change it if another repo is a \
                         better fit for this task."
                    )
                } else {
                    format!(
                        "`{top}` is furthest upstream: {n} other {} depend{} on contracts it \
                         defines, so it can settle the interface before anyone else needs it.",
                        if n == 1 { "repository" } else { "repositories" },
                        if n == 1 { "s" } else { "" }
                    )
                };
                (top.clone(), reason)
            }
            None => (String::new(), "no repositories in this project".into()),
        }
    }

    /// Cycle the master through the project's repos — §5.7 requires the election be editable.
    pub fn cycle_master(&mut self, repos: &[String], delta: isize) {
        if repos.is_empty() {
            return;
        }
        let cur = repos.iter().position(|r| *r == self.master).unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(repos.len() as isize) as usize;
        self.master = repos[next].clone();
        self.master_cursor = next;
        self.master_reason = format!("`{}` — chosen by you.", self.master);
    }

    // ---- text entry --------------------------------------------------------

    pub fn push_char(&mut self, c: char) {
        match self.step {
            Step::ProjectName => self.project_name.push(c),
            Step::TaskBrief => self.brief.push(c),
            Step::PickRepos if self.filtering => {
                self.filter.push(c);
                self.cursor = 0;
            }
            _ => {}
        }
    }

    pub fn backspace(&mut self) {
        match self.step {
            Step::ProjectName => {
                self.project_name.pop();
            }
            Step::TaskBrief => {
                self.brief.pop();
            }
            Step::PickRepos if self.filtering => {
                self.filter.pop();
                self.cursor = 0;
            }
            _ => {}
        }
    }

    /// Whether the current step can be confirmed.
    pub fn can_advance(&self) -> bool {
        match self.step {
            Step::ProjectName => !self.project_name.trim().is_empty(),
            Step::PickRepos => !self.selected.is_empty(),
            Step::TaskBrief => !self.brief.trim().is_empty(),
            Step::ConfirmMaster => !self.master.is_empty(),
            _ => true,
        }
    }

    /// A one-line reason the current step cannot advance, for the footer.
    pub fn blocked_because(&self) -> Option<&'static str> {
        match self.step {
            Step::ProjectName if !self.can_advance() => Some("a project needs a name"),
            Step::PickRepos if !self.can_advance() => {
                Some("select at least one repository with space")
            }
            Step::TaskBrief if !self.can_advance() => Some("describe the task first"),
            _ => None,
        }
    }
}

/// Scan for repositories and time it (§5.3).
pub fn scan_repos(config: &reporch_store::Config) -> (Vec<FoundRepo>, f64) {
    let opts = reporch_scan::ScanOptions {
        roots: config.scan_roots(),
        max_depth: config.scan.max_depth,
        skip: config.scan.skip.iter().cloned().collect(),
        threads: 0,
    };
    let t = std::time::Instant::now();
    let found = reporch_scan::scan(&opts);
    (found, t.elapsed().as_secs_f64())
}

/// Where a repo's analysis profile and its prose form are written (§8).
pub fn write_profile(
    paths: &reporch_store::Paths,
    project: &str,
    profile: &reporch_core::RepoProfile,
) -> Result<(), reporch_store::StoreError> {
    reporch_store::atomic::write_json(&paths.repo_profile_json(project, &profile.name), profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::{GraphNode, ProjectRepo};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn repo(name: &str, parent: &str, org: Option<&str>) -> FoundRepo {
        FoundRepo {
            name: name.into(),
            path: PathBuf::from(parent).join(name),
            remote_url: org.map(|o| format!("git@github.com:{o}/{name}.git")),
            remote_org: org.map(str::to_string),
            head_age_secs: None,
        }
    }

    fn wizard_with_repos() -> Wizard {
        let mut w = Wizard::new();
        w.project_name = "modulex".into();
        w.candidates = vec![
            repo("modulex", "/repos", Some("example-org")),
            repo("aaa-unrelated", "/elsewhere", None),
            repo("keymaster", "/repos", Some("example-org")),
        ];
        w.step = Step::PickRepos;
        w.rerank();
        w
    }

    /// §5.3's promise: choosing one repo immediately pulls the family up.
    #[test]
    fn selecting_a_repo_reranks_the_list_immediately() {
        let mut w = wizard_with_repos();
        let before = w
            .visible()
            .iter()
            .position(|r| r.repo.name == "keymaster")
            .unwrap();

        // Select `modulex` — which is first, so cursor 0.
        assert_eq!(w.visible()[0].repo.name, "modulex");
        w.toggle_current();

        let after = w
            .visible()
            .iter()
            .position(|r| r.repo.name == "keymaster")
            .unwrap();
        assert!(
            after < before,
            "a shared remote org must promote it: before {before}, after {after}"
        );
    }

    #[test]
    fn toggling_selects_and_deselects() {
        let mut w = wizard_with_repos();
        w.toggle_current();
        assert_eq!(w.selected.len(), 1);
        // The list reordered, so move back to it by name rather than by index.
        let idx = w
            .visible()
            .iter()
            .position(|r| w.selected.contains(&r.repo.name))
            .unwrap();
        w.cursor = idx;
        w.toggle_current();
        assert!(w.selected.is_empty());
    }

    #[test]
    fn the_cursor_wraps_and_stays_in_range() {
        let mut w = wizard_with_repos();
        w.move_cursor(-1);
        assert_eq!(w.cursor, 2, "up from the top wraps to the bottom");
        w.move_cursor(1);
        assert_eq!(w.cursor, 0);
        // Filtering to a shorter list must not leave the cursor out of bounds.
        w.cursor = 2;
        w.filter = "modulex".into();
        w.rerank();
        assert!(w.cursor < w.visible().len().max(1));
    }

    #[test]
    fn filtering_narrows_the_list() {
        let mut w = wizard_with_repos();
        w.filtering = true;
        for c in "keym".chars() {
            w.push_char(c);
        }
        let names: Vec<&str> = w.visible().iter().map(|r| r.repo.name.as_str()).collect();
        assert_eq!(names, vec!["keymaster"]);
        w.backspace();
        w.backspace();
        w.backspace();
        w.backspace();
        assert_eq!(w.visible().len(), 3);
    }

    #[test]
    fn a_step_cannot_advance_until_it_is_valid() {
        let mut w = Wizard::new();
        assert!(!w.can_advance(), "an unnamed project cannot proceed");
        assert!(w.blocked_because().is_some());
        w.project_name = "modulex".into();
        assert!(w.can_advance());

        w.step = Step::PickRepos;
        assert!(!w.can_advance(), "no repos selected");
        w.selected.insert("modulex".into());
        assert!(w.can_advance());

        w.step = Step::TaskBrief;
        assert!(!w.can_advance());
        w.brief = "Add org-scoped API keys.".into();
        assert!(w.can_advance());
    }

    #[test]
    fn whitespace_alone_is_not_a_valid_name_or_brief() {
        let mut w = Wizard::new();
        w.project_name = "   \n ".into();
        assert!(!w.can_advance());
        w.step = Step::TaskBrief;
        w.brief = "\t\n  ".into();
        assert!(!w.can_advance());
    }

    // ---- §5.6 -------------------------------------------------------------

    #[test]
    fn the_effort_screen_produces_the_spec_ladder() {
        let mut w = Wizard::new();
        for (i, (master, worker)) in [
            (Effort::Low, Effort::Low),
            (Effort::Medium, Effort::Low),
            (Effort::High, Effort::Medium),
            (Effort::XHigh, Effort::High),
            (Effort::Max, Effort::XHigh),
        ]
        .into_iter()
        .enumerate()
        {
            w.effort_cursor = i;
            let plan = w.effort_plan();
            assert_eq!(plan.master, master);
            assert_eq!(plan.worker, worker);
        }
    }

    #[test]
    fn ultracode_is_master_only() {
        let mut w = Wizard::new();
        w.effort_cursor = 4;
        w.ultracode = true;
        let plan = w.effort_plan();
        assert_eq!(plan.master, Effort::Max);
        assert_eq!(plan.worker, Effort::XHigh);
        assert!(plan.ultracode);
        assert!(!plan.ultracode_for_role(reporch_core::Role::Worker));
    }

    #[test]
    fn the_default_effort_is_a_sensible_middle() {
        let w = Wizard::new();
        assert_eq!(w.effort(), Effort::XHigh);
        assert_eq!(w.model(), "opus", "§5.6: one model for every repo");
    }

    #[test]
    fn model_choices_track_current_claude_aliases() {
        let aliases: Vec<_> = MODELS.iter().map(|(alias, _)| *alias).collect();
        assert_eq!(
            aliases,
            vec![
                "opus",
                "best",
                "fable",
                "fable[1m]",
                "opus[1m]",
                "sonnet",
                "sonnet[1m]",
                "haiku",
            ],
            "§5.6: expose durable aliases and the native long-context choices"
        );
        assert!(
            MODELS
                .iter()
                .find(|(alias, _)| *alias == "best")
                .is_some_and(|(_, description)| description.contains("Fable 5")),
            "the safest Fable entry is Claude's best alias, which falls back to Opus"
        );
        assert!(
            MODELS
                .iter()
                .filter(|(alias, _)| alias.ends_with("[1m]"))
                .all(|(_, description)| description.contains("usage credits")),
            "non-interactive 1M sessions must warn about their account-dependent billing"
        );
        assert!(MODELS
            .iter()
            .filter(|(alias, _)| alias.starts_with("fable"))
            .all(|(_, description)| description.contains("requires organization access")));
    }

    // ---- §5.7 -------------------------------------------------------------

    fn project(graph: BTreeMap<String, GraphNode>, names: &[&str]) -> ProjectBase {
        ProjectBase {
            schema_version: 1,
            name: "modulex".into(),
            created_at: "t".into(),
            analyzed_at: None,
            overview: String::new(),
            repositories: names
                .iter()
                .map(|n| ProjectRepo {
                    name: n.to_string(),
                    path: format!("/r/{n}"),
                    remote_url: None,
                    default_branch: None,
                    purpose: String::new(),
                    profile_ref: None,
                    cross_session_inbound: None,
                })
                .collect(),
            graph,
            settings: Default::default(),
            next_task_id: 1,
        }
    }

    #[test]
    fn the_election_is_biased_upstream_and_explains_itself() {
        let mut g = BTreeMap::new();
        g.insert(
            "modulex".to_string(),
            GraphNode {
                consumes: vec![],
                produces_contracts_for: vec!["modulex-ui".into(), "modulex-js".into()],
            },
        );
        let p = project(g, &["modulex", "modulex-ui", "modulex-js"]);
        let (master, reason) = Wizard::elect_from_graph(&p);
        assert_eq!(master, "modulex");
        assert!(reason.contains("furthest upstream"), "{reason}");
        assert!(reason.contains("2 other repositories"), "{reason}");
    }

    #[test]
    fn with_no_graph_the_election_says_it_is_arbitrary() {
        let p = project(BTreeMap::new(), &["a", "b"]);
        let (master, reason) = Wizard::elect_from_graph(&p);
        assert_eq!(master, "a");
        assert!(
            reason.contains("alphabetical"),
            "must not pretend to know: {reason}"
        );
        assert!(reason.contains("Change it"), "{reason}");
    }

    /// §5.7: the election is shown and **editable** before the task starts.
    #[test]
    fn the_master_can_be_changed_by_the_user() {
        let repos: Vec<String> = ["modulex", "modulex-ui", "modulex-js"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut w = Wizard::new();
        w.master = "modulex".into();

        w.cycle_master(&repos, 1);
        assert_eq!(w.master, "modulex-ui");
        assert!(w.master_reason.contains("chosen by you"));

        w.cycle_master(&repos, -1);
        assert_eq!(w.master, "modulex");
        // Wraps.
        w.cycle_master(&repos, -1);
        assert_eq!(w.master, "modulex-js");
    }

    #[test]
    fn cycling_with_no_repos_does_nothing() {
        let mut w = Wizard::new();
        w.cycle_master(&[], 1);
        assert_eq!(w.master, "");
    }

    #[test]
    fn a_task_wizard_starts_at_the_brief() {
        let mut p = project(BTreeMap::new(), &["a"]);
        p.settings.default_model = "haiku".into();
        let w = Wizard::for_task(&p);
        assert_eq!(w.step, Step::TaskBrief);
        assert_eq!(w.project_name, "modulex");
        assert_eq!(
            w.model(),
            "haiku",
            "the task picker must start on the analysed project's configured default"
        );
    }
}
