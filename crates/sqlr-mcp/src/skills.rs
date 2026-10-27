//! The sqlr skills, embedded from the repository's `skills/` folder so the app and the
//! repository ship the same files. The Terminal workspace writes them out; the tests below
//! keep them in step with the tool registry and the settings catalogue.

/// One file of a skill.
pub struct SkillFile {
    /// The path inside the skill folder, `/`-separated (`SKILL.md`, `references/sql.md`).
    pub path: &'static str,
    pub contents: &'static str,
}

/// One sqlr skill: its folder name and every file in it.
pub struct Skill {
    pub name: &'static str,
    pub files: &'static [SkillFile],
}

/// A file of `skills/<skill>/<path>`, embedded at build time.
macro_rules! skill_file {
    ($skill:literal, $path:literal) => {
        SkillFile {
            path: $path,
            contents: include_str!(concat!("../../../skills/", $skill, "/", $path)),
        }
    };
}

/// The embedded sqlr skills.
pub static SKILLS: &[Skill] = &[
    Skill {
        name: "sqlr-configure",
        files: &[skill_file!("sqlr-configure", "SKILL.md")],
    },
    Skill {
        name: "sqlr-create-connection",
        files: &[skill_file!("sqlr-create-connection", "SKILL.md")],
    },
    Skill {
        name: "sqlr-edit-user-query",
        files: &[skill_file!("sqlr-edit-user-query", "SKILL.md")],
    },
    Skill {
        name: "sqlr-query-database",
        files: &[
            skill_file!("sqlr-query-database", "SKILL.md"),
            skill_file!("sqlr-query-database", "references/sql.md"),
            skill_file!("sqlr-query-database", "references/mongodb.md"),
            skill_file!("sqlr-query-database", "references/elasticsearch.md"),
            skill_file!("sqlr-query-database", "references/redis.md"),
        ],
    },
    Skill {
        name: "sqlr-connect-agent",
        files: &[skill_file!("sqlr-connect-agent", "SKILL.md")],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};

    /// Backticked identifiers that have a tool's shape (a registry verb followed by
    /// `_words`) but name something else.
    const NOT_TOOLS: &[&str] = &["search_path"];

    /// The inline code spans of a Markdown text, outside fenced blocks.
    fn code_spans(text: &str) -> Vec<&str> {
        let mut spans = Vec::new();
        let mut fenced = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if !fenced {
                spans.extend(line.split('`').skip(1).step_by(2));
            }
        }
        spans
    }

    /// `verb_words`: lowercase snake case with at least one underscore.
    fn is_snake_case(token: &str) -> bool {
        let mut words = token.split('_');
        let first_ok = words
            .next()
            .is_some_and(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()));
        let mut rest = 0;
        let rest_ok = words.all(|w| {
            rest += 1;
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
        first_ok && rest_ok && rest > 0
    }

    /// `section.key[.key…]`, optionally ending in `.*`.
    fn is_dotted_key(token: &str) -> bool {
        let (token, glob) = match token.strip_suffix(".*") {
            Some(prefix) => (prefix, true),
            None => (token, false),
        };
        if !glob && !token.contains('.') {
            return false;
        }
        let mut segments = token.split('.');
        let first = segments.next().unwrap_or_default();
        let segment_ok = |s: &str| {
            s.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        segment_ok(first) && segments.all(segment_ok)
    }

    /// The `key: value` pairs of a `SKILL.md` frontmatter; nested keys are joined with `.`.
    fn frontmatter(text: &str) -> Option<Vec<(String, String)>> {
        let body = text.strip_prefix("---\n")?;
        let end = body.find("\n---\n")?;
        let mut pairs = Vec::new();
        let mut parent: Option<String> = None;
        for line in body[..end].lines() {
            let nested = line.starts_with(' ');
            let (key, value) = line.trim().split_once(':')?;
            let key = key.trim().to_owned();
            let value = value.trim().to_owned();
            if nested {
                pairs.push((format!("{}.{key}", parent.clone()?), value));
            } else if value.is_empty() {
                parent = Some(key);
            } else {
                parent = None;
                pairs.push((key, value));
            }
        }
        Some(pairs)
    }

    /// Everything that is out of step between `skills`, the tool names `tools` and the
    /// settings catalogue.
    fn problems(tools: &[&str], skills: &[Skill]) -> Vec<String> {
        let mut problems = Vec::new();
        let verbs: HashSet<&str> = tools
            .iter()
            .filter_map(|tool| tool.split('_').next())
            .collect();
        let sections: HashSet<&str> = sqlr_core::settings_model::setting_defs()
            .iter()
            .filter_map(|def| def.id.split('.').next())
            .collect();
        let mut cited = HashSet::new();
        for skill in skills {
            for file in skill.files {
                let at = format!("{}/{}", skill.name, file.path);
                for span in code_spans(file.contents) {
                    cited.insert(span);
                    if is_snake_case(span)
                        && span.split('_').next().is_some_and(|v| verbs.contains(v))
                        && !NOT_TOOLS.contains(&span)
                        && !tools.contains(&span)
                    {
                        problems.push(format!("{at} names unknown tool `{span}`"));
                    }
                    if is_dotted_key(span)
                        && span.split('.').next().is_some_and(|s| sections.contains(s))
                    {
                        let known = match span.strip_suffix(".*") {
                            Some(prefix) => sqlr_core::settings_model::setting_defs()
                                .iter()
                                .any(|def| def.id.starts_with(&format!("{prefix}."))),
                            None => sqlr_core::settings_model::setting_def(span).is_some(),
                        };
                        if !known {
                            problems.push(format!("{at} names unknown setting `{span}`"));
                        }
                    }
                }
            }
        }
        for tool in tools {
            if !cited.contains(tool) {
                problems.push(format!("no skill names tool `{tool}`"));
            }
        }
        problems
    }

    fn registry_names() -> Vec<&'static str> {
        crate::registry::TOOLS
            .iter()
            .map(|tool| tool.name)
            .collect()
    }

    #[test]
    fn the_five_sqlr_skills_are_embedded() {
        let names: Vec<&str> = SKILLS.iter().map(|skill| skill.name).collect();
        assert_eq!(
            names,
            [
                "sqlr-configure",
                "sqlr-create-connection",
                "sqlr-edit-user-query",
                "sqlr-query-database",
                "sqlr-connect-agent",
            ]
        );
    }

    #[test]
    fn every_file_in_the_skills_folder_is_embedded() {
        fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut BTreeSet<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    let relative = path.strip_prefix(root).unwrap();
                    let parts: Vec<_> = relative
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect();
                    out.insert(parts.join("/"));
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let mut on_disk = BTreeSet::new();
        walk(&root, &root, &mut on_disk);
        let embedded: BTreeSet<String> = SKILLS
            .iter()
            .flat_map(|skill| {
                skill
                    .files
                    .iter()
                    .map(move |file| format!("{}/{}", skill.name, file.path))
            })
            .collect();
        assert_eq!(embedded, on_disk);
    }

    #[test]
    fn every_skill_has_valid_frontmatter_with_the_crate_version() {
        for skill in SKILLS {
            let manifest = skill
                .files
                .iter()
                .find(|file| file.path == "SKILL.md")
                .unwrap_or_else(|| panic!("{} has no SKILL.md", skill.name));
            let pairs = frontmatter(manifest.contents)
                .unwrap_or_else(|| panic!("{} has no valid frontmatter", skill.name));
            let get = |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.as_str())
            };
            assert_eq!(get("name"), Some(skill.name));
            assert!(skill.name.len() <= 64);
            let description = get("description").unwrap_or_default();
            assert!(
                !description.is_empty() && description.len() <= 1024,
                "{} description",
                skill.name
            );
            assert_eq!(
                get("metadata.sqlr-version"),
                Some(env!("CARGO_PKG_VERSION")),
                "{} sqlr-version",
                skill.name
            );
        }
    }

    #[test]
    fn the_skills_cover_the_registry_and_cite_only_real_tools_and_settings() {
        assert_eq!(problems(&registry_names(), SKILLS), Vec::<String>::new());
    }

    #[test]
    fn a_tool_no_skill_names_is_reported() {
        let mut tools = registry_names();
        tools.push("drop_everything");
        assert_eq!(
            problems(&tools, SKILLS),
            ["no skill names tool `drop_everything`"]
        );
    }

    #[test]
    fn unknown_tools_and_settings_in_a_skill_are_reported() {
        static FILES: &[SkillFile] = &[SkillFile {
            path: "SKILL.md",
            contents: "Call `get_guide`, then `get_snippet` and `run_query` with `query_id`, set `grid.page_size` \
                       and `grid.pages`, read `drivers.*` and `nothing.*`, `search_path`.\n\
                       ```\n`list_everything`\n```\n",
        }];
        static FAKE: &[Skill] = &[Skill {
            name: "fake",
            files: FILES,
        }];
        let problems = problems(&["run_query", "get_snippet", "list_snippets"], FAKE);
        assert_eq!(
            problems,
            [
                "fake/SKILL.md names unknown tool `get_guide`",
                "fake/SKILL.md names unknown setting `grid.pages`",
                "no skill names tool `list_snippets`",
            ]
        );
    }
}
