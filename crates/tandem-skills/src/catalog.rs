use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

/// Stable identifier of a skill: its `name` in SKILL.md frontmatter, which equals its directory
/// name and the directory it is materialized into (`.agents/skills/<id>/`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SkillId(pub String);

impl SkillId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for SkillId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl From<&str> for SkillId {
    fn from(s: &str) -> Self {
        SkillId(s.to_string())
    }
}
impl From<String> for SkillId {
    fn from(s: String) -> Self {
        SkillId(s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub id: SkillId,
    pub name: String,
    pub description: String,
    /// Embedded path relative to the `skills/` root, e.g. `gstack/gstack-review`.
    pub path: String,
    /// Skills the pack marks as always-on (persistent modes such as ponytail or caveman):
    /// the index block tells the agent to read them at session start, not "when relevant".
    pub always_on: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pack {
    pub id: String,
    pub name: String,
    pub description: String,
    /// SPDX identifier of the pack as vendored.
    pub license: String,
    pub upstream: String,
    pub skills: Vec<Skill>,
}

#[derive(Deserialize)]
struct PackMeta {
    id: String,
    name: String,
    description: String,
    license: String,
    #[serde(default)]
    upstream: String,
    #[serde(default)]
    always_on: Vec<String>,
}

#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}

/// Split `---\nyaml\n---\nbody`. Returns `None` when there is no frontmatter block.
pub(crate) fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    let yaml = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.strip_prefix("\r\n").or_else(|| after.strip_prefix('\n')).unwrap_or(after);
    Some((yaml, body))
}

/// Parse `(name, description)` out of a SKILL.md.
pub(crate) fn parse_skill_md(text: &str) -> anyhow::Result<(String, String)> {
    let (yaml, _) = split_frontmatter(text).ok_or_else(|| anyhow::anyhow!("missing YAML frontmatter"))?;
    let fm: Frontmatter = serde_yaml::from_str(yaml)?;
    anyhow::ensure!(!fm.name.trim().is_empty(), "empty name");
    anyhow::ensure!(!fm.description.trim().is_empty(), "empty description");
    Ok((fm.name, fm.description))
}

fn build() -> Vec<Pack> {
    // pack id -> skill dir -> present
    let mut metas: BTreeMap<&str, PackMeta> = BTreeMap::new();
    let mut skill_mds: BTreeMap<&str, Vec<(&str, &[u8])>> = BTreeMap::new();
    for (rel, bytes) in embedded::FILES {
        let parts: Vec<&str> = rel.split('/').collect();
        match parts.as_slice() {
            [pack, "pack.json"] => match serde_json::from_slice::<PackMeta>(bytes) {
                Ok(m) => {
                    metas.insert(pack, m);
                }
                Err(e) => tracing::warn!(pack, error = %e, "bad pack.json"),
            },
            [pack, dir, "SKILL.md"] => skill_mds.entry(pack).or_default().push((dir, bytes)),
            _ => {}
        }
    }
    let mut packs = Vec::new();
    for (pack_dir, meta) in metas {
        let mut skills = Vec::new();
        for (dir, bytes) in skill_mds.remove(pack_dir).unwrap_or_default() {
            let text = String::from_utf8_lossy(bytes);
            match parse_skill_md(&text) {
                Ok((name, description)) => skills.push(Skill {
                    always_on: meta.always_on.iter().any(|a| a == &name),
                    id: SkillId(name.clone()),
                    name,
                    description,
                    path: format!("{pack_dir}/{dir}"),
                }),
                Err(e) => tracing::warn!(pack = pack_dir, skill = dir, error = %e, "skipping unparsable SKILL.md"),
            }
        }
        skills.sort_by(|a, b| a.id.cmp(&b.id));
        packs.push(Pack {
            id: meta.id,
            name: meta.name,
            description: meta.description,
            license: meta.license,
            upstream: meta.upstream,
            skills,
        });
    }
    packs
}

/// All bundled packs, sorted by directory name. Parsed once, then cloned.
pub fn catalog() -> Vec<Pack> {
    static CATALOG: OnceLock<Vec<Pack>> = OnceLock::new();
    CATALOG.get_or_init(build).clone()
}

/// Look a skill up by id across every pack.
pub fn find_skill(id: &SkillId) -> Option<Skill> {
    catalog().into_iter().flat_map(|p| p.skills).find(|s| &s.id == id)
}

/// Every embedded file of a skill as `(path relative to the skill dir, bytes)`.
pub fn skill_files(skill: &Skill) -> Vec<(&'static str, &'static [u8])> {
    let prefix = format!("{}/", skill.path);
    embedded::FILES
        .iter()
        .filter_map(|(rel, bytes)| rel.strip_prefix(prefix.as_str()).map(|r| (r, *bytes)))
        .collect()
}

/// Raw embedded files (test support).
#[cfg(test)]
pub(crate) fn embedded_files() -> &'static [(&'static str, &'static [u8])] {
    embedded::FILES
}
