use super::*;
use crate::catalog::{embedded_files, parse_skill_md};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn tree(root: &Path) -> BTreeSet<String> {
    fn go(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            out.insert(rel);
            if e.file_type().unwrap().is_dir() {
                go(&p, root, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    go(root, root, &mut out);
    out
}

#[test]
fn catalog_has_expected_packs() {
    let c = catalog();
    assert!(c.len() >= 4, "got {} packs", c.len());
    for id in ["superpowers", "ponytail", "caveman", "gstack", "tandem"] {
        let p = c.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("missing pack {id}"));
        assert!(!p.skills.is_empty(), "{id} has no skills");
        assert!(!p.license.is_empty());
    }
}

#[test]
fn every_skill_md_parses_with_valid_name() {
    let mut n = 0;
    for (rel, bytes) in embedded_files() {
        let parts: Vec<&str> = rel.split('/').collect();
        if let [pack, dir, "SKILL.md"] = parts.as_slice() {
            let text = std::str::from_utf8(bytes).unwrap_or_else(|_| panic!("{rel} not utf-8"));
            let (name, desc) = parse_skill_md(text).unwrap_or_else(|e| panic!("{rel}: {e}"));
            // Source dir may drop the pack prefix (`tandem/review` -> `tandem-review`); the
            // materialized dir is always the name.
            assert!(&name == dir || name == format!("{pack}-{dir}"), "{rel}: name `{name}` does not match dir");
            assert!(name.len() <= 64 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{rel}: bad name");
            assert!(desc.chars().count() <= 1024, "{rel}: description {} chars", desc.chars().count());
            n += 1;
        }
    }
    let in_catalog: usize = catalog().iter().map(|p| p.skills.len()).sum();
    assert_eq!(n, in_catalog, "a SKILL.md failed to load into the catalog");
    assert!(n >= 25);
}

#[test]
fn skill_ids_are_unique_and_presets_resolve() {
    let mut seen = BTreeSet::new();
    for s in catalog().into_iter().flat_map(|p| p.skills) {
        assert!(seen.insert(s.id.clone()), "duplicate skill id {}", s.id);
    }
    for p in Preset::ALL {
        let ids = p.skills();
        assert!(!ids.is_empty());
        for id in ids {
            assert!(find_skill(&id).is_some(), "preset {:?} references unknown skill {id}", p);
        }
    }
}

#[test]
fn every_pack_ships_a_license() {
    for p in catalog() {
        assert!(embedded_files().iter().any(|(r, _)| *r == format!("{}/LICENSE", p.id)), "{} has no LICENSE", p.id);
    }
}

#[test]
fn materialize_is_idempotent_and_clean_removes_everything() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path();
    let sel = Preset::Lazy.skills();
    let targets = [AgentTarget::ClaudeCode, AgentTarget::Codex, AgentTarget::Kiro];

    let r = materialize(wt, &sel, &targets).unwrap();
    assert_eq!(r.skills.len(), 2);
    for id in &sel {
        for base in [".claude/skills", ".agents/skills", ".kiro/skills"] {
            assert!(wt.join(base).join(&id.0).join("SKILL.md").is_file(), "{base}/{id}");
        }
    }
    assert!(!wt.join(".gemini").exists());
    for f in ["AGENTS.md", "CLAUDE.md", ".kiro/steering/tandem-skills.md"] {
        let t = fs::read_to_string(wt.join(f)).unwrap();
        assert!(t.contains(BLOCK_START) && t.contains(BLOCK_END), "{f}");
        assert!(t.contains("ponytail"));
        assert!(t.contains("Read `.agents/skills/ponytail/SKILL.md`"));
    }

    let first = tree(wt);
    let agents_first = fs::read_to_string(wt.join("AGENTS.md")).unwrap();
    materialize(wt, &sel, &targets).unwrap();
    assert_eq!(first, tree(wt), "second run changed the file set");
    assert_eq!(agents_first, fs::read_to_string(wt.join("AGENTS.md")).unwrap());
    assert_eq!(agents_first.matches(BLOCK_START).count(), 1);

    clean(wt).unwrap();
    assert!(tree(wt).is_empty(), "left behind: {:?}", tree(wt));
}

#[test]
fn existing_agents_md_is_preserved_and_block_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path();
    fs::write(wt.join("AGENTS.md"), "# Project\n\nBe nice.\n").unwrap();
    materialize(wt, &Preset::Lazy.skills(), &[AgentTarget::ClaudeCode]).unwrap();
    materialize(wt, &Preset::Terse.skills(), &[AgentTarget::ClaudeCode]).unwrap();
    let t = fs::read_to_string(wt.join("AGENTS.md")).unwrap();
    assert!(t.starts_with("# Project\n\nBe nice.\n"));
    assert_eq!(t.matches(BLOCK_START).count(), 1);
    assert!(t.contains("caveman") && !t.contains("ponytail"));
    // switching presets removed the stale skills
    assert!(!wt.join(".claude/skills/ponytail").exists());
    assert!(wt.join(".claude/skills/caveman/SKILL.md").exists());

    clean(wt).unwrap();
    assert_eq!(fs::read_to_string(wt.join("AGENTS.md")).unwrap(), "# Project\n\nBe nice.\n");
    assert!(!wt.join(".claude").exists());
}

#[test]
fn user_owned_skill_dirs_are_never_touched() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path();
    fs::create_dir_all(wt.join(".claude/skills/ponytail")).unwrap();
    fs::write(wt.join(".claude/skills/ponytail/SKILL.md"), "mine").unwrap();
    let r = materialize(wt, &Preset::Lazy.skills(), &[AgentTarget::ClaudeCode]).unwrap();
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(fs::read_to_string(wt.join(".claude/skills/ponytail/SKILL.md")).unwrap(), "mine");
    clean(wt).unwrap();
    assert_eq!(fs::read_to_string(wt.join(".claude/skills/ponytail/SKILL.md")).unwrap(), "mine");
}

#[cfg(unix)]
#[test]
fn symlink_mode_links_to_canonical_copy() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path();
    let opts = MaterializeOptions { symlink: true };
    materialize_with(wt, &Preset::Lazy.skills(), &[AgentTarget::ClaudeCode], opts).unwrap();
    let link = wt.join(".claude/skills/ponytail");
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert!(link.join("SKILL.md").is_file());
    materialize_with(wt, &Preset::Lazy.skills(), &[AgentTarget::ClaudeCode], opts).unwrap();
    clean(wt).unwrap();
    assert!(tree(wt).is_empty(), "left behind: {:?}", tree(wt));
}

#[test]
fn nested_files_and_exec_bits_survive() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path();
    materialize(wt, &["systematic-debugging".into(), "gstack-careful".into()], &[]).unwrap();
    assert!(wt.join(".agents/skills/systematic-debugging/root-cause-tracing.md").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = fs::metadata(wt.join(".agents/skills/gstack-careful/bin/check-careful.sh")).unwrap();
        assert!(m.permissions().mode() & 0o111 != 0);
    }
}

#[test]
fn unknown_skill_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(materialize(dir.path(), &["nope".into()], &[]).is_err());
}
