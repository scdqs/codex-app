use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{codex_rpc::CodexThread, protocol::WorkspaceOption};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkspaceValidationError {
    #[error("workspace is required")]
    Required,
    #[error("workspace is not allowed")]
    NotAllowed,
    #[error("workspace is unavailable")]
    Unavailable,
}

pub fn workspace_options(threads: &[CodexThread]) -> Vec<WorkspaceOption> {
    let workspaces = threads
        .iter()
        .filter_map(|thread| thread.cwd.as_deref())
        .filter_map(canonical_workspace)
        .map(|cwd| cwd.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();

    workspaces
        .into_iter()
        .map(|cwd| WorkspaceOption { cwd })
        .collect()
}

pub fn validate_workspace(
    threads: &[CodexThread],
    requested_cwd: Option<&str>,
) -> Result<WorkspaceOption, WorkspaceValidationError> {
    let cwd = requested_cwd.ok_or(WorkspaceValidationError::Required)?;
    if !is_allowed_path_shape(cwd) {
        return Err(WorkspaceValidationError::NotAllowed);
    }

    let canonical_cwd = match fs::canonicalize(cwd) {
        Ok(canonical_cwd) => canonical_cwd,
        Err(_) => {
            return if threads
                .iter()
                .any(|thread| thread.cwd.as_deref() == Some(cwd))
            {
                Err(WorkspaceValidationError::Unavailable)
            } else {
                Err(WorkspaceValidationError::NotAllowed)
            };
        }
    };
    if canonical_cwd == Path::new("/") {
        return Err(WorkspaceValidationError::NotAllowed);
    }
    if !is_existing_directory(&canonical_cwd) {
        return Err(WorkspaceValidationError::Unavailable);
    }

    if !threads
        .iter()
        .filter_map(|thread| thread.cwd.as_deref())
        .filter_map(canonical_workspace)
        .any(|allowed_cwd| allowed_cwd == canonical_cwd)
    {
        return Err(WorkspaceValidationError::NotAllowed);
    }

    Ok(WorkspaceOption {
        cwd: canonical_cwd.to_string_lossy().into_owned(),
    })
}

fn is_allowed_path_shape(cwd: &str) -> bool {
    let path = Path::new(cwd);
    path.is_absolute() && path != Path::new("/")
}

fn canonical_workspace(cwd: &str) -> Option<PathBuf> {
    if !is_allowed_path_shape(cwd) {
        return None;
    }

    let canonical_cwd = fs::canonicalize(cwd).ok()?;
    if canonical_cwd == Path::new("/") || !is_existing_directory(&canonical_cwd) {
        return None;
    }
    Some(canonical_cwd)
}

fn is_existing_directory(cwd: &Path) -> bool {
    fs::metadata(cwd)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::codex_rpc::CodexThread;

    use super::{WorkspaceValidationError, validate_workspace, workspace_options};

    fn canonical_string(path: &std::path::Path) -> String {
        fs::canonicalize(path)
            .expect("canonical path")
            .to_string_lossy()
            .into_owned()
    }

    fn thread(id: &str, cwd: Option<String>) -> CodexThread {
        CodexThread {
            id: id.to_string(),
            title: None,
            cwd,
            model_provider: None,
            preview: None,
            created_at: None,
            updated_at: None,
            raw: json!({}),
        }
    }

    #[test]
    fn workspace_options_keep_existing_absolute_directories_and_sort_deduplicated_paths() {
        let temp = tempdir().expect("tempdir");
        let alpha = temp.path().join("alpha");
        let zeta = temp.path().join("zeta");
        let file = temp.path().join("notes.txt");
        fs::create_dir(&alpha).expect("alpha directory");
        fs::create_dir(&zeta).expect("zeta directory");
        fs::write(&file, "not a workspace").expect("workspace file");

        let threads = vec![
            thread("zeta", Some(zeta.to_string_lossy().into_owned())),
            thread("alpha", Some(alpha.to_string_lossy().into_owned())),
            thread("duplicate", Some(alpha.to_string_lossy().into_owned())),
            thread("root", Some("/".to_string())),
            thread("relative", Some("relative/project".to_string())),
            thread("file", Some(file.to_string_lossy().into_owned())),
            thread(
                "missing",
                Some(temp.path().join("missing").to_string_lossy().into_owned()),
            ),
            thread("none", None),
        ];

        let options = workspace_options(&threads);

        assert_eq!(
            options
                .into_iter()
                .map(|option| option.cwd)
                .collect::<Vec<_>>(),
            vec![canonical_string(&alpha), canonical_string(&zeta),]
        );
    }

    #[test]
    fn validate_workspace_distinguishes_required_not_allowed_and_unavailable_paths() {
        let temp = tempdir().expect("tempdir");
        let valid = temp.path().join("valid");
        let disappearing = temp.path().join("disappearing");
        fs::create_dir(&valid).expect("valid directory");
        fs::create_dir(&disappearing).expect("disappearing directory");
        let threads = vec![
            thread("valid", Some(valid.to_string_lossy().into_owned())),
            thread(
                "disappearing",
                Some(disappearing.to_string_lossy().into_owned()),
            ),
        ];

        assert_eq!(
            validate_workspace(&threads, None),
            Err(WorkspaceValidationError::Required)
        );
        assert_eq!(
            validate_workspace(&threads, Some("/")),
            Err(WorkspaceValidationError::NotAllowed)
        );
        assert_eq!(
            validate_workspace(&threads, Some("relative/project")),
            Err(WorkspaceValidationError::NotAllowed)
        );
        assert_eq!(
            validate_workspace(
                &threads,
                Some(temp.path().join("other").to_string_lossy().as_ref()),
            ),
            Err(WorkspaceValidationError::NotAllowed)
        );

        fs::remove_dir(&disappearing).expect("remove disappearing directory");
        assert_eq!(
            validate_workspace(&threads, Some(disappearing.to_string_lossy().as_ref())),
            Err(WorkspaceValidationError::Unavailable)
        );

        assert_eq!(
            validate_workspace(&threads, Some(valid.to_string_lossy().as_ref()))
                .expect("valid workspace")
                .cwd,
            canonical_string(&valid)
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_paths_are_canonicalized_before_root_checks_and_deduplication() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let project = temp.path().join("project");
        let project_alias = temp.path().join("project-alias");
        let root_alias = temp.path().join("root-alias");
        fs::create_dir(&project).expect("project directory");
        symlink(&project, &project_alias).expect("project symlink");
        symlink("/", &root_alias).expect("root symlink");

        let threads = vec![
            thread("project", Some(project.to_string_lossy().into_owned())),
            thread(
                "project-alias",
                Some(project_alias.to_string_lossy().into_owned()),
            ),
            thread(
                "root-alias",
                Some(root_alias.to_string_lossy().into_owned()),
            ),
        ];

        assert_eq!(
            workspace_options(&threads),
            vec![crate::protocol::WorkspaceOption {
                cwd: canonical_string(&project),
            }]
        );
        assert_eq!(
            validate_workspace(&threads, Some(root_alias.to_string_lossy().as_ref())),
            Err(WorkspaceValidationError::NotAllowed)
        );
        assert_eq!(
            validate_workspace(&threads, Some(project_alias.to_string_lossy().as_ref()))
                .expect("canonical project alias")
                .cwd,
            canonical_string(&project)
        );
    }
}
