use std::{fs, io, path::Path};

use globset::{Glob, GlobMatcher};

/// Source of an ignore decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IgnoreSource {
    /// Built-in exclusion that Git-tracked files may override.
    Default,
    /// Explicit project rule that always wins.
    Explicit,
}

/// Result of checking a project-relative path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IgnoreDecision {
    /// Whether the path is excluded.
    pub ignored: bool,
    /// Rule source when excluded.
    pub source: Option<IgnoreSource>,
    /// Matching pattern when excluded.
    pub pattern: Option<String>,
}

impl IgnoreDecision {
    fn included() -> Self {
        Self {
            ignored: false,
            source: None,
            pattern: None,
        }
    }

    /// Returns true when a Git-tracked file may override a default exclusion.
    #[must_use]
    pub const fn overridable_by_tracking(&self) -> bool {
        self.ignored && matches!(self.source, Some(IgnoreSource::Default))
    }
}

/// Built-in and `.agenttracebackignore` exclusion rules.
#[derive(Clone, Debug)]
pub struct IgnoreEngine {
    default_rules: Vec<(String, GlobMatcher)>,
    explicit_rules: Vec<(String, GlobMatcher)>,
}

impl IgnoreEngine {
    /// Loads defaults and optional `.agenttracebackignore` from the project root.
    pub fn load(project_root: &Path) -> io::Result<Self> {
        let default_patterns = [
            "**/.git/**",
            "**/node_modules/**",
            "**/target/**",
            "**/dist/**",
            "**/build/**",
            "**/.cache/**",
            "**/.venv/**",
            "**/venv/**",
            "**/coverage/**",
            "**/.agenttraceback/**",
        ];
        let mut default_rules = compile_rules(default_patterns.into_iter().map(str::to_owned))?;
        let ignore_path = project_root.join(".agenttracebackignore");
        let mut explicit_rules = Vec::new();
        if ignore_path.is_file() {
            let contents = fs::read_to_string(ignore_path)?;
            explicit_rules = compile_rules(contents.lines().filter_map(normalize_rule))?;
        }
        default_rules.shrink_to_fit();
        Ok(Self {
            default_rules,
            explicit_rules,
        })
    }

    /// Checks a project-relative slash-separated path.
    #[must_use]
    pub fn check(&self, relative_path: &str) -> IgnoreDecision {
        if let Some((pattern, _)) = self
            .explicit_rules
            .iter()
            .find(|(_, matcher)| matcher.is_match(relative_path))
        {
            return IgnoreDecision {
                ignored: true,
                source: Some(IgnoreSource::Explicit),
                pattern: Some(pattern.clone()),
            };
        }
        if let Some((pattern, _)) = self
            .default_rules
            .iter()
            .find(|(_, matcher)| matcher.is_match(relative_path))
        {
            return IgnoreDecision {
                ignored: true,
                source: Some(IgnoreSource::Default),
                pattern: Some(pattern.clone()),
            };
        }
        IgnoreDecision::included()
    }
}

fn normalize_rule(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let normalized = trimmed.trim_start_matches('/');
    if normalized.ends_with('/') {
        Some(format!("**/{normalized}**"))
    } else if normalized.contains('/') || normalized.contains('*') {
        Some(normalized.to_owned())
    } else {
        Some(format!("**/{normalized}"))
    }
}

fn compile_rules(
    patterns: impl IntoIterator<Item = String>,
) -> io::Result<Vec<(String, GlobMatcher)>> {
    patterns
        .into_iter()
        .map(|pattern| {
            let glob = Glob::new(&pattern)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            Ok((pattern, glob.compile_matcher()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{IgnoreEngine, IgnoreSource};

    #[test]
    fn defaults_can_be_overridden_but_explicit_rules_cannot() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(
            directory.path().join(".agenttracebackignore"),
            "generated/**\n",
        )
        .expect("ignore file");
        let engine = IgnoreEngine::load(directory.path()).expect("ignore engine");

        let default = engine.check("node_modules/pkg/index.js");
        assert!(default.overridable_by_tracking());
        let explicit = engine.check("generated/schema.rs");
        assert_eq!(explicit.source, Some(IgnoreSource::Explicit));
        assert!(!explicit.overridable_by_tracking());
    }
}
