use crate::{AgentTarget, Skill, SkillId, find_skill, skill_files};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const BLOCK_START: &str = "<!-- tandem:skills:start -->";
pub const BLOCK_END: &str = "<!-- tandem:skills:end -->";
/// Marker file written into every directory Tandem owns, so `clean` and re-sync never touch
/// a user's own skill that happens to live next to ours. Review tooling should ignore it.
const MARKER: &str = ".tandem-skill";
/// Canonical copy; the index block always points here regardless of the chosen targets.
const CANONICAL: &str = ".agents/skills";
const INDEX_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

#[derive(Clone, Copy, Debug, Default)]
pub struct MaterializeOptions {
    /// Non-canonical target dirs become relative symlinks to `.agents/skills/<id>` instead of
    /// copies (unix only; copies elsewhere). Copies are the default because they survive tools
    /// that do not follow links and keep worktrees self-contained.
    pub symlink: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaterializeReport {
    pub skills: Vec<SkillId>,
    /// Skill directories now present (relative to the worktree).
    pub dirs: Vec<PathBuf>,
    /// Index files written or updated (relative to the worktree).
    pub index_files: Vec<PathBuf>,
    /// Paths left alone because a user-owned entry already exists there.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Relative paths (to the worktree) Tandem may create, for review/diff tooling to filter out.
pub fn managed_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = AgentTarget::ALL.iter().map(|t| t.skills_dir()).collect();
    v.extend(AgentTarget::ALL.iter().filter_map(|t| t.steering_file()));
    v.sort();
    v.dedup();
    v
}

/// Copy the selected skills into the worktree for each target, and upsert the index block into
/// `AGENTS.md` and `CLAUDE.md`. Idempotent; previously materialized skills that are no longer
/// selected are removed. `.agents/skills` is always written.
pub fn materialize(worktree: &Path, selection: &[SkillId], targets: &[AgentTarget]) -> Result<MaterializeReport> {
    materialize_with(worktree, selection, targets, MaterializeOptions::default())
}

pub fn materialize_with(
    worktree: &Path,
    selection: &[SkillId],
    targets: &[AgentTarget],
    opts: MaterializeOptions,
) -> Result<MaterializeReport> {
    let mut skills: Vec<Skill> = Vec::new();
    let mut seen = BTreeSet::new();
    for id in selection {
        if !seen.insert(id.clone()) {
            continue;
        }
        match find_skill(id) {
            Some(s) => skills.push(s),
            None => bail!("unknown skill id `{id}`"),
        }
    }
    let mut report = MaterializeReport { skills: skills.iter().map(|s| s.id.clone()).collect(), ..Default::default() };

    let mut dirs: Vec<PathBuf> = vec![PathBuf::from(CANONICAL)];
    for t in targets {
        let d = t.skills_dir();
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    for dir in &dirs {
        let canonical = dir == Path::new(CANONICAL);
        sync_dir(worktree, dir, &skills, opts.symlink && !canonical, &mut report)?;
    }
    // Targets that were deselected earlier keep stale copies otherwise; sweep every known dir.
    for t in AgentTarget::ALL {
        let d = t.skills_dir();
        if !dirs.contains(&d) {
            remove_managed(&worktree.join(&d), &BTreeSet::new())?;
        }
    }

    let block = render_index_block(&skills);
    for f in INDEX_FILES {
        if skills.is_empty() {
            remove_block(&worktree.join(f))?;
        } else if upsert_block(&worktree.join(f), &block)? {
            report.index_files.push(PathBuf::from(f));
        }
    }
    for t in AgentTarget::ALL {
        if let Some(steer) = t.steering_file() {
            let p = worktree.join(&steer);
            if targets.contains(&t) && !skills.is_empty() {
                write_if_changed(&p, format!("{block}\n").as_bytes(), false)?;
                report.index_files.push(steer);
            } else if p.exists() && is_owned_steering(&p) {
                fs::remove_file(&p)?;
                prune_empty(p.parent().unwrap(), worktree);
            }
        }
    }
    Ok(report)
}

/// Remove everything `materialize` created: managed skill dirs for every known target, the
/// index blocks (deleting `AGENTS.md`/`CLAUDE.md` if nothing else is left in them), and empty
/// parent directories.
pub fn clean(worktree: &Path) -> Result<()> {
    for t in AgentTarget::ALL {
        let d = worktree.join(t.skills_dir());
        remove_managed(&d, &BTreeSet::new())?;
        prune_empty(&d, worktree);
        if let Some(steer) = t.steering_file() {
            let p = worktree.join(steer);
            if p.exists() && is_owned_steering(&p) {
                fs::remove_file(&p)?;
                prune_empty(p.parent().unwrap(), worktree);
            }
        }
    }
    for f in INDEX_FILES {
        remove_block(&worktree.join(f))?;
    }
    Ok(())
}

/// The markdown block listing skills, wrapped in the start/end markers (no trailing newline).
pub fn render_index_block(skills: &[Skill]) -> String {
    let mut s = String::new();
    s.push_str(BLOCK_START);
    s.push_str("\n## Skills (managed by Tandem)\n\n");
    s.push_str(
        "Skills are folders containing a `SKILL.md`. When a task matches one, read its file and follow it. \
         This block is generated; edit nothing between the markers.\n\n",
    );
    for sk in skills {
        let desc = one_line(&sk.description, 160);
        if sk.always_on {
            s.push_str(&format!(
                "- **{id}** (always on): {desc} Read `.agents/skills/{id}/SKILL.md` now and follow it for the whole session.\n",
                id = sk.id
            ));
        } else {
            s.push_str(&format!(
                "- **{id}**: {desc} Read `.agents/skills/{id}/SKILL.md` when relevant.\n",
                id = sk.id
            ));
        }
    }
    s.push_str(BLOCK_END);
    s
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > max / 2 => cut[..i].to_string(),
        _ => cut,
    };
    format!("{}...", cut.trim_end_matches(['.', ',', ';', ':', ' ']))
}

// ---------- skill directories ----------

fn sync_dir(
    worktree: &Path,
    rel_dir: &Path,
    skills: &[Skill],
    symlink: bool,
    report: &mut MaterializeReport,
) -> Result<()> {
    let dir = worktree.join(rel_dir);
    let keep: BTreeSet<String> = skills.iter().map(|s| s.id.0.clone()).collect();
    remove_managed(&dir, &keep)?;
    for sk in skills {
        let dest = dir.join(&sk.id.0);
        let rel = rel_dir.join(&sk.id.0);
        if fs::symlink_metadata(&dest).is_ok() && !is_managed(&dest) {
            report.skipped.push((rel, "exists and is not managed by Tandem".into()));
            continue;
        }
        if symlink && cfg!(unix) {
            link_skill(worktree, rel_dir, sk, &dest)?;
        } else {
            copy_skill(sk, &dest)?;
        }
        report.dirs.push(rel);
    }
    Ok(())
}

fn copy_skill(sk: &Skill, dest: &Path) -> Result<()> {
    if let Ok(md) = fs::symlink_metadata(dest)
        && md.file_type().is_symlink()
    {
        fs::remove_file(dest)?;
    }
    let files = skill_files(sk);
    let wanted: BTreeSet<&str> = files.iter().map(|(r, _)| *r).collect();
    // Drop files that disappeared from the embedded skill (upgrade), keep the rest untouched.
    if dest.is_dir() {
        for existing in walk_files(dest)? {
            let rel = existing.strip_prefix(dest).unwrap().to_string_lossy().replace('\\', "/");
            if rel != MARKER && !wanted.contains(rel.as_str()) {
                fs::remove_file(&existing)?;
            }
        }
    }
    for (rel, bytes) in files {
        write_if_changed(&dest.join(rel), bytes, bytes.starts_with(b"#!"))?;
    }
    write_if_changed(&dest.join(MARKER), sk.id.0.as_bytes(), false)?;
    prune_empty_subdirs(dest);
    Ok(())
}

#[cfg(unix)]
fn link_skill(worktree: &Path, rel_dir: &Path, sk: &Skill, dest: &Path) -> Result<()> {
    // Make sure the canonical copy exists, then point at it relatively.
    let canon = worktree.join(CANONICAL).join(&sk.id.0);
    if !canon.exists() {
        copy_skill(sk, &canon)?;
    }
    let up = "../".repeat(rel_dir.components().count());
    let target = format!("{up}{CANONICAL}/{}", sk.id.0);
    if let Ok(cur) = fs::read_link(dest)
        && cur == Path::new(&target)
    {
        return Ok(());
    }
    remove_any(dest)?;
    fs::create_dir_all(dest.parent().unwrap())?;
    std::os::unix::fs::symlink(&target, dest).with_context(|| format!("symlink {}", dest.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn link_skill(_: &Path, _: &Path, sk: &Skill, dest: &Path) -> Result<()> {
    copy_skill(sk, dest)
}

fn is_managed(p: &Path) -> bool {
    match fs::symlink_metadata(p) {
        Ok(md) if md.file_type().is_symlink() => fs::read_link(p)
            .map(|t| t.to_string_lossy().replace('\\', "/").contains(&format!("{CANONICAL}/")))
            .unwrap_or(false),
        Ok(md) if md.is_dir() => p.join(MARKER).is_file(),
        _ => false,
    }
}

/// Remove managed children of `dir` whose name is not in `keep`.
fn remove_managed(dir: &Path, keep: &BTreeSet<String>) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else { return Ok(()) };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) {
            continue;
        }
        let p = e.path();
        if is_managed(&p) {
            remove_any(&p)?;
        }
    }
    Ok(())
}

fn remove_any(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(md) if md.is_dir() => fs::remove_dir_all(p)?,
        Ok(_) => fs::remove_file(p)?,
        Err(_) => {}
    }
    Ok(())
}

fn walk_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)?.flatten() {
        let p = e.path();
        if e.file_type()?.is_dir() {
            out.extend(walk_files(&p)?);
        } else {
            out.push(p);
        }
    }
    Ok(out)
}

fn prune_empty_subdirs(dir: &Path) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                prune_empty_subdirs(&e.path());
                let _ = fs::remove_dir(e.path());
            }
        }
    }
}

/// Remove `dir` and its parents up to (not including) `stop`, while empty.
fn prune_empty(dir: &Path, stop: &Path) {
    let mut cur = dir.to_path_buf();
    while cur != stop && cur.starts_with(stop) {
        if fs::remove_dir(&cur).is_err() {
            break;
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => break,
        }
    }
}

fn write_if_changed(path: &Path, bytes: &[u8], exec: bool) -> Result<bool> {
    if fs::read(path).map(|cur| cur == bytes).unwrap_or(false) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    if exec {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = exec;
    Ok(true)
}

// ---------- index blocks ----------

fn is_owned_steering(p: &Path) -> bool {
    fs::read_to_string(p).map(|t| t.trim_start().starts_with(BLOCK_START)).unwrap_or(false)
}

fn find_block(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BLOCK_START)?;
    let end_rel = text[start..].find(BLOCK_END)?;
    Some((start, start + end_rel + BLOCK_END.len()))
}

/// Insert or replace the marked block. Returns true when the file now carries the block.
fn upsert_block(path: &Path, block: &str) -> Result<bool> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let new = match find_block(&existing) {
        Some((s, e)) => format!("{}{}{}", &existing[..s], block, &existing[e..]),
        None if existing.trim().is_empty() => format!("{block}\n"),
        None => {
            let mut t = existing.trim_end_matches('\n').to_string();
            t.push_str("\n\n");
            t.push_str(block);
            t.push('\n');
            t
        }
    };
    write_if_changed(path, new.as_bytes(), false)?;
    Ok(true)
}

fn remove_block(path: &Path) -> Result<()> {
    let Ok(existing) = fs::read_to_string(path) else { return Ok(()) };
    let Some((s, e)) = find_block(&existing) else { return Ok(()) };
    let before = existing[..s].trim_end_matches('\n');
    let after = existing[e..].trim_start_matches('\n');
    let mut out = String::from(before);
    if !before.is_empty() && !after.is_empty() {
        out.push_str("\n\n");
    } else if !before.is_empty() {
        out.push('\n');
    }
    out.push_str(after);
    if out.trim().is_empty() {
        fs::remove_file(path)?;
    } else {
        fs::write(path, out)?;
    }
    Ok(())
}
