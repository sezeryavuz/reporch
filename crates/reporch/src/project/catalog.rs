//! Read persisted projects and task history for the console (§5.5, §8.2, §8.4).

use reporch_core::{Task, TaskStatus};
use reporch_store::{atomic, Paths, TaskPaths};

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRecord {
    pub task: Task,
    pub paths: TaskPaths,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskCatalog {
    pub tasks: Vec<TaskRecord>,
    pub warnings: Vec<String>,
}

impl TaskCatalog {
    pub fn running(&self) -> impl Iterator<Item = &TaskRecord> {
        self.tasks
            .iter()
            .filter(|record| record.task.status.is_running())
    }

    pub fn completed(&self) -> impl Iterator<Item = &TaskRecord> {
        self.tasks
            .iter()
            .filter(|record| record.task.status.is_mergeable())
    }

    pub fn history(&self) -> impl Iterator<Item = &TaskRecord> {
        self.tasks.iter().filter(|record| {
            matches!(
                record.task.status,
                TaskStatus::Merged
                    | TaskStatus::Cancelled
                    | TaskStatus::Delayed
                    | TaskStatus::Failed
            )
        })
    }

    pub fn all_finished(&self) -> impl Iterator<Item = &TaskRecord> {
        self.tasks
            .iter()
            .filter(|record| !record.task.status.is_running())
    }
}

/// Load every readable task newest first. One corrupt task is surfaced but does not hide the
/// rest of the project — history is a debugging surface, especially when a run failed.
pub fn load_tasks(paths: &Paths, project: &str) -> TaskCatalog {
    let root = paths.tasks_dir(project);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return TaskCatalog::default();
    };
    let mut catalog = TaskCatalog::default();
    for entry in entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
    {
        let task_paths = TaskPaths::at(entry.path());
        match atomic::read_json::<Task>(&task_paths.task_json()) {
            Ok(task) => catalog.tasks.push(TaskRecord {
                task,
                paths: task_paths,
            }),
            Err(error) => catalog
                .warnings
                .push(format!("{}: {error}", entry.file_name().to_string_lossy())),
        }
    }
    catalog
        .tasks
        .sort_by_key(|record| std::cmp::Reverse(record.task.id.number()));
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;
    use reporch_core::{Effort, TaskId};

    fn task(id: u32, status: TaskStatus) -> Task {
        Task {
            schema_version: 1,
            id: TaskId::new(id),
            name: format!("task-{id}"),
            status,
            created_at: "2026-08-12T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            master_repo: "api".into(),
            model: "sonnet".into(),
            master_effort: Effort::Medium,
            worker_effort: Effort::Low,
            ultracode: false,
            budget_usd: 1.0,
            spent_usd: 0.2,
            branch: format!("reporch/TASK-{id:04}-task"),
            repositories: Vec::new(),
        }
    }

    #[test]
    fn persisted_tasks_are_sorted_and_bucketed_without_hiding_a_bad_record() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::at(temp.path());
        let root = paths.tasks_dir("project");
        for value in [
            task(1, TaskStatus::Merged),
            task(3, TaskStatus::Completed),
            task(2, TaskStatus::Active),
        ] {
            let dir = root.join(format!("{}-{}", value.name, value.id));
            std::fs::create_dir_all(&dir).unwrap();
            atomic::write_json(&dir.join("task.json"), &value).unwrap();
        }
        let bad = root.join("broken-TASK-0004");
        std::fs::create_dir_all(&bad).unwrap();
        atomic::write_string(&bad.join("task.json"), "not json").unwrap();

        let catalog = load_tasks(&paths, "project");

        assert_eq!(
            catalog
                .tasks
                .iter()
                .map(|record| record.task.id.number())
                .collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert_eq!(catalog.running().count(), 1);
        assert_eq!(catalog.completed().count(), 1);
        assert_eq!(catalog.history().count(), 1);
        assert_eq!(catalog.warnings.len(), 1);
    }
}
