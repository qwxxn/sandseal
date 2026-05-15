use anyhow::Result;
use std::env;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Expand environment variables in a string.
/// Supports `$VAR` and `${VAR}` syntax. Undefined variables are left unexpanded with a warning.
pub fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' {
            let braced = chars.peek() == Some(&'{');
            if braced {
                chars.next();
            }

            let mut var_name = String::new();
            while let Some(&c) = chars.peek() {
                if braced {
                    if c == '}' {
                        chars.next();
                        break;
                    }
                } else if !c.is_alphanumeric() && c != '_' {
                    break;
                }
                var_name.push(c);
                chars.next();
            }

            if var_name.is_empty() {
                result.push('$');
                if braced {
                    result.push('{');
                    result.push('}');
                }
            } else {
                match env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        warn!("undefined environment variable: {var_name}");
                        result.push('$');
                        if braced {
                            result.push('{');
                        }
                        result.push_str(&var_name);
                        if braced {
                            result.push('}');
                        }
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Expand tilde prefix to home directory.
pub fn expand_tilde(input: &str) -> String {
    if input == "~" {
        return dirs::home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| input.to_string());
    }
    if let Some(rest) = input.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|p| format!("{}/{rest}", p.display()))
            .unwrap_or_else(|| input.to_string());
    }
    input.to_string()
}

/// Full path resolution: env vars → tilde → relative-to-absolute.
/// Relative paths are resolved against `base_dir`.
pub fn resolve_host_path(input: &str, base_dir: &Path) -> PathBuf {
    let expanded = expand_env_vars(input);
    let expanded = expand_tilde(&expanded);
    let path = PathBuf::from(&expanded);

    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}

/// Resolve a container path: env vars → tilde (using sandbox home) → absolute.
pub fn resolve_container_path(input: &str, sandbox_home: &str, project_dir: &Path) -> PathBuf {
    let expanded = expand_env_vars(input);

    let expanded = if expanded == "~" {
        sandbox_home.to_string()
    } else if let Some(rest) = expanded.strip_prefix("~/") {
        format!("{sandbox_home}/{rest}")
    } else {
        expanded
    };

    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    }
}

/// Expand glob patterns against a source directory.
/// Uses bash-style globstar (`**` for recursive matching).
pub fn expand_glob(pattern: &str, source_dir: &Path) -> Result<Vec<PathBuf>> {
    let full_pattern = if PathBuf::from(pattern).is_absolute() {
        pattern.to_string()
    } else {
        format!("{}/{pattern}", source_dir.display())
    };

    let paths: Vec<PathBuf> = glob::glob(&full_pattern)?
        .filter_map(|entry| entry.ok())
        .collect();

    Ok(paths)
}

/// Check if a string contains glob metacharacters.
pub fn has_glob_chars(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// Strip trailing slashes from a path string.
pub fn strip_trailing_slash(s: &str) -> &str {
    s.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expansion() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~/projects"), format!("{}/projects", home.display()));
        assert_eq!(expand_tilde("~"), home.to_string_lossy().to_string());
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn env_var_expansion() {
        unsafe { env::set_var("SANDSEAL_TEST_VAR", "hello") };
        assert_eq!(expand_env_vars("$SANDSEAL_TEST_VAR/world"), "hello/world");
        assert_eq!(expand_env_vars("${SANDSEAL_TEST_VAR}/world"), "hello/world");
        unsafe { env::remove_var("SANDSEAL_TEST_VAR") };
    }

    #[test]
    fn glob_chars_detection() {
        assert!(has_glob_chars(".env*"));
        assert!(has_glob_chars("**/*.rs"));
        assert!(has_glob_chars("file[0-9]"));
        assert!(!has_glob_chars(".env"));
        assert!(!has_glob_chars("node_modules"));
    }

    #[test]
    fn trailing_slash_strip() {
        assert_eq!(strip_trailing_slash("node_modules/"), "node_modules");
        assert_eq!(strip_trailing_slash("dir///"), "dir");
        assert_eq!(strip_trailing_slash("file"), "file");
    }

    #[test]
    fn resolve_relative_path() {
        let base = PathBuf::from("/home/user/project");
        assert_eq!(resolve_host_path("./config", &base), PathBuf::from("/home/user/project/./config"));
        assert_eq!(resolve_host_path("/absolute", &base), PathBuf::from("/absolute"));
    }
}
