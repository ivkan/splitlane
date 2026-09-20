use std::path::{Path, PathBuf};

/// Resolve the cwd to use when the caller did not request one explicitly.
///
/// GUI launches can inherit the filesystem root as the process cwd on macOS.
/// Treat that as an unhelpful implicit cwd and prefer the user's home dir,
/// while still preserving an explicitly requested `/` or drive root because
/// those paths bypass this helper.
pub(crate) fn implicit_launch_cwd() -> PathBuf {
    resolve_implicit_launch_cwd(std::env::current_dir().ok(), dirs::home_dir())
}

/// The directory the first container opens on when there is none to restore,
/// or `None` for no container at all.
///
/// Only a launch from a terminal names a directory: somebody typed the command
/// standing in it. A launch from Finder, the Dock, a `.desktop` file or the
/// Start menu names none - the cwd it inherits is `/` on macOS and the home
/// folder elsewhere - and the home folder is not a project. Opening it as one
/// handed every agent started there the whole of `~`, and on macOS an agent
/// that indexes its working directory then walks Desktop, Documents, the
/// Photos library and Music, each one a permission prompt naming this app.
pub(crate) fn first_container_cwd() -> Option<PathBuf> {
    use std::io::IsTerminal;
    first_container_cwd_for(std::io::stdin().is_terminal(), implicit_launch_cwd())
}

fn first_container_cwd_for(launched_from_terminal: bool, cwd: PathBuf) -> Option<PathBuf> {
    launched_from_terminal.then_some(cwd)
}

pub(crate) fn title_for_cwd_or(cwd: &Path, fallback: impl Into<String>) -> String {
    cwd.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fallback.into())
}

fn resolve_implicit_launch_cwd(current: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    match current {
        Some(current) if is_filesystem_root(&current) => home.unwrap_or(current),
        Some(current) => current,
        None => home.unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR.to_string())),
    }
}

pub(crate) fn is_filesystem_root(path: &Path) -> bool {
    path.has_root() && path.parent().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform_root() -> PathBuf {
        std::env::current_dir()
            .ok()
            .and_then(|path| path.ancestors().last().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR.to_string()))
    }

    fn child_of_root(name: &str) -> PathBuf {
        let mut path = platform_root();
        path.push(name);
        path
    }

    #[test]
    fn implicit_launch_cwd_uses_home_when_current_is_root() {
        let root = platform_root();
        let home = child_of_root("home");

        assert_eq!(
            resolve_implicit_launch_cwd(Some(root), Some(home.clone())),
            home
        );
    }

    #[test]
    fn implicit_launch_cwd_keeps_non_root_current() {
        let current = child_of_root("project");
        let home = child_of_root("home");

        assert_eq!(
            resolve_implicit_launch_cwd(Some(current.clone()), Some(home)),
            current
        );
    }

    #[test]
    fn implicit_launch_cwd_uses_home_when_current_is_missing() {
        let home = child_of_root("home");

        assert_eq!(resolve_implicit_launch_cwd(None, Some(home.clone())), home);
    }

    #[test]
    fn title_for_cwd_or_uses_last_path_component() {
        let cwd = child_of_root("splitlane");

        assert_eq!(title_for_cwd_or(&cwd, "Terminal 1"), "splitlane");
    }

    #[test]
    fn title_for_cwd_or_falls_back_for_root() {
        let root = platform_root();

        assert_eq!(title_for_cwd_or(&root, "Terminal 1"), "Terminal 1");
    }

    #[test]
    fn a_launch_from_a_terminal_opens_the_directory_it_was_started_in() {
        let cwd = child_of_root("project");

        assert_eq!(first_container_cwd_for(true, cwd.clone()), Some(cwd));
    }

    #[test]
    fn a_launch_from_the_desktop_opens_no_project() {
        let home = child_of_root("home");

        assert_eq!(first_container_cwd_for(false, home), None);
    }
}
