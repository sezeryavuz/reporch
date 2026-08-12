//! The console's live task actor (§5.8, §13.3).
//!
//! Worktree creation is blocking and happens on Tokio's blocking pool. Once sessions exist, this
//! task is the single owner of [`Supervisor`]; session pumps and the UI communicate with it only
//! through channels. Dropping the session `JoinSet` aborts every pump, whose `ChildTransport`
//! then kills its process via `kill_on_drop` — the panic/cancellation backstop against orphans.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use reporch_claude::{ChildTransport, SessionEvent};
use reporch_core::{ProjectBase, Task, TaskStatus};
use reporch_store::{atomic, Config, Paths, TaskPaths};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::{create_task, kickoff_prompt, CreateRequest, Created, RunView, Supervisor};

/// Everything selected in the console before any worktree is created.
#[derive(Debug, Clone)]
pub struct TaskRunRequest {
    pub paths: Paths,
    pub config: Config,
    pub project: ProjectBase,
    pub create: CreateRequest,
}

/// Task identity becomes available after blocking preparation and before the first paid turn.
#[derive(Debug, Clone)]
pub struct StartedTask {
    pub task: Task,
    pub paths: TaskPaths,
    pub warnings: Vec<String>,
}

/// Observable task-runtime transitions consumed by the console.
#[derive(Debug, Clone)]
pub enum TaskRunEvent {
    Preparing,
    Started(Box<StartedTask>),
    View(RunView),
    Failed(String),
    Stopped,
}

/// Commands from the run screen to its task actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRunCommand {
    Abort,
    SetPaused(bool),
}

/// Prepare, spawn and supervise one task until completion, cancellation or failure.
pub async fn run_task(
    request: TaskRunRequest,
    events: mpsc::UnboundedSender<TaskRunEvent>,
    mut commands: mpsc::UnboundedReceiver<TaskRunCommand>,
) {
    let _ = events.send(TaskRunEvent::Preparing);
    let TaskRunRequest {
        paths,
        config,
        project,
        create,
    } = request;

    let prepared = tokio::task::spawn_blocking(move || {
        create_task(&paths, &config, &project, &create).map(|created| (created, config, project))
    })
    .await;
    let (created, config, project) = match prepared {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            let _ = events.send(TaskRunEvent::Failed(error.to_string()));
            return;
        }
        Err(error) => {
            let _ = events.send(TaskRunEvent::Failed(format!(
                "task preparation panicked or was cancelled: {error}"
            )));
            return;
        }
    };

    run_created(created, config, project, events, &mut commands).await;
}

async fn run_created(
    created: Created,
    config: Config,
    project: ProjectBase,
    events: mpsc::UnboundedSender<TaskRunEvent>,
    commands: &mut mpsc::UnboundedReceiver<TaskRunCommand>,
) {
    let Created {
        mut task,
        paths,
        specs,
        warnings,
    } = created;
    let (session_events_tx, mut session_events_rx) = mpsc::unbounded_channel::<SessionEvent>();
    let mut injectors = BTreeMap::new();
    let mut sessions = JoinSet::new();

    for spec in specs {
        let transport = match ChildTransport::spawn(&reporch_claude::spawn::build(&spec)) {
            Ok(transport) => transport,
            Err(error) => {
                fail_before_supervisor(
                    &mut task,
                    &paths,
                    &events,
                    format!("could not spawn `{}`: {error}", spec.repo),
                    &mut sessions,
                )
                .await;
                return;
            }
        };
        if let Some(repo) = task.repo_mut(&spec.repo) {
            repo.session_pid = transport.pid();
        }
        let (inject_tx, inject_rx) = mpsc::unbounded_channel::<String>();
        injectors.insert(spec.repo.clone(), inject_tx);
        let repo = spec.repo.clone();
        let task_repo = repo.clone();
        let log = paths.session_log(&repo);
        let events = session_events_tx.clone();
        sessions.spawn(async move {
            let result = reporch_claude::run_session(
                task_repo.clone(),
                transport,
                inject_rx,
                events,
                Some(log),
            )
            .await;
            (task_repo, result)
        });
    }
    drop(session_events_tx);

    task.started_at = Some(reporch_store::now_rfc3339());
    if let Err(error) = atomic::write_json(&paths.task_json(), &task) {
        fail_before_supervisor(
            &mut task,
            &paths,
            &events,
            format!("could not persist spawned sessions: {error}"),
            &mut sessions,
        )
        .await;
        return;
    }

    let master_injector = injectors.get(&task.master_repo).cloned();
    let mut supervisor = Supervisor::new(
        task.clone(),
        paths.clone(),
        config.limits.to_ping_limits(),
        injectors,
    );

    let kickoff = kickoff_prompt(&task, &create_brief(&paths), &project);
    if let Err(error) = atomic::write_string(&paths.prompt_md(), &kickoff) {
        fail_live(
            &mut supervisor,
            &events,
            format!("could not write the kickoff prompt: {error}"),
            &mut sessions,
        )
        .await;
        return;
    }
    let Some(master_injector) = master_injector else {
        fail_live(
            &mut supervisor,
            &events,
            "the elected master has no session injector".into(),
            &mut sessions,
        )
        .await;
        return;
    };
    if master_injector
        .send(reporch_claude::spawn::injection_line(&kickoff))
        .is_err()
    {
        fail_live(
            &mut supervisor,
            &events,
            "the elected master exited before kickoff".into(),
            &mut sessions,
        )
        .await;
        return;
    }
    drop(master_injector);
    let _ = events.send(TaskRunEvent::Started(Box::new(StartedTask {
        task: task.clone(),
        paths: paths.clone(),
        warnings,
    })));

    let start = Instant::now();
    let timeout = Duration::from_secs(config.limits.task_timeout_secs);
    let mut ticker = tokio::time::interval(super::supervisor::TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let _ = events.send(TaskRunEvent::View(supervisor.view()));

    loop {
        tokio::select! {
            Some(event) = session_events_rx.recv() => {
                supervisor.on_session_event(event, elapsed_ms(start));
            }
            _ = ticker.tick() => {
                supervisor.tick(elapsed_ms(start));
                if start.elapsed() >= timeout {
                    supervisor.delay(format!(
                        "task reached its configured wall-clock ceiling of {}s; sessions stopped \
                         and worktrees were preserved",
                        timeout.as_secs()
                    ));
                }
            }
            command = commands.recv() => {
                match command {
                    None | Some(TaskRunCommand::Abort) => {
                        supervisor.cancel();
                        let _ = events.send(TaskRunEvent::View(supervisor.view()));
                        abort_sessions(&mut sessions).await;
                        let _ = events.send(TaskRunEvent::Stopped);
                        return;
                    }
                    Some(TaskRunCommand::SetPaused(paused)) => {
                        supervisor.set_paused(paused);
                        if !paused {
                            supervisor.tick(elapsed_ms(start));
                        }
                    }
                }
            }
            joined = sessions.join_next(), if !sessions.is_empty() => {
                match joined {
                    Some(Ok((repo, Err(error)))) => {
                        supervisor.on_session_event(
                            SessionEvent::Exited { repo: repo.clone(), code: None },
                            elapsed_ms(start),
                        );
                        tracing::warn!(%repo, %error, "session pump failed");
                    }
                    Some(Err(error)) => {
                        fail_live(
                            &mut supervisor,
                            &events,
                            format!("a session pump panicked: {error}"),
                            &mut sessions,
                        ).await;
                        return;
                    }
                    Some(Ok((_, Ok(())))) | None => {}
                }
            }
        }

        let view = supervisor.view();
        let finished = view.finished.is_some();
        let _ = events.send(TaskRunEvent::View(view));
        if finished {
            break;
        }
    }

    supervisor.shutdown();
    graceful_sessions(&mut sessions).await;
    let _ = events.send(TaskRunEvent::Stopped);
}

fn create_brief(paths: &TaskPaths) -> String {
    atomic::read_string(&paths.base_md()).unwrap_or_default()
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

async fn fail_before_supervisor(
    task: &mut Task,
    paths: &TaskPaths,
    events: &mpsc::UnboundedSender<TaskRunEvent>,
    error: String,
    sessions: &mut JoinSet<(String, std::io::Result<()>)>,
) {
    task.status = TaskStatus::Failed;
    task.finished_at = Some(reporch_store::now_rfc3339());
    let _ = atomic::write_json(&paths.task_json(), task);
    let _ = events.send(TaskRunEvent::Failed(error));
    abort_sessions(sessions).await;
}

async fn fail_live(
    supervisor: &mut Supervisor,
    events: &mpsc::UnboundedSender<TaskRunEvent>,
    error: String,
    sessions: &mut JoinSet<(String, std::io::Result<()>)>,
) {
    supervisor.fail(error.clone());
    let _ = events.send(TaskRunEvent::View(supervisor.view()));
    let _ = events.send(TaskRunEvent::Failed(error));
    abort_sessions(sessions).await;
}

async fn graceful_sessions(sessions: &mut JoinSet<(String, std::io::Result<()>)>) {
    let drain = async { while sessions.join_next().await.is_some() {} };
    if tokio::time::timeout(
        reporch_claude::session::SHUTDOWN_GRACE + Duration::from_secs(5),
        drain,
    )
    .await
    .is_err()
    {
        abort_sessions(sessions).await;
    }
}

async fn abort_sessions(sessions: &mut JoinSet<(String, std::io::Result<()>)>) {
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::{Effort, EffortPlan};

    #[tokio::test]
    async fn preparation_failure_is_terminal_and_spawns_no_session() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::at(temp.path().join("home"));
        let project = ProjectBase {
            schema_version: 1,
            name: "example-app".into(),
            created_at: "2026-08-12T00:00:00Z".into(),
            analyzed_at: Some("2026-08-12T00:00:01Z".into()),
            overview: String::new(),
            repositories: Vec::new(),
            graph: BTreeMap::new(),
            // Deliberately no consent: create_task must stop before worktrees or paid sessions.
            settings: Default::default(),
            next_task_id: 1,
        };
        let request = TaskRunRequest {
            paths,
            config: Config::default(),
            project,
            create: CreateRequest {
                project: "example-app".into(),
                brief: "Add widget status.".into(),
                slug: "add-widget-status".into(),
                model: "sonnet".into(),
                effort: EffortPlan::new(Effort::Low, false),
                master: "example-app".into(),
                budget_usd_per_session: 0.50,
            },
        };
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let (_commands_tx, commands_rx) = mpsc::unbounded_channel();

        run_task(request, events_tx, commands_rx).await;

        assert!(matches!(
            events_rx.recv().await,
            Some(TaskRunEvent::Preparing)
        ));
        match events_rx.recv().await {
            Some(TaskRunEvent::Failed(error)) => {
                assert!(error.contains("has not consented"), "{error}");
            }
            other => panic!("expected terminal preparation failure, got {other:?}"),
        }
        assert!(events_rx.recv().await.is_none());
    }
}
