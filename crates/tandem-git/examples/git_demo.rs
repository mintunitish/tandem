//! `cargo run -p tandem-git --example git_demo -- <path>`: status + diff summary of the repo at `path`.
use tandem_core::{FileChange, LineKind};
use tandem_git::Repo;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let repo = Repo::open(&path)?;
    println!("root:   {}", repo.root().display());
    println!("branch: {}", repo.current_branch()?.unwrap_or_else(|| "(detached)".into()));
    if let Some(c) = repo.head_commit()? {
        println!("head:   {} {}", c.short_id, c.summary);
    }
    println!("\nstatus:");
    for e in repo.status()? {
        println!("  idx={:?} wt={:?}  {}", e.index, e.worktree, e.path.display());
    }
    println!("\ndiff vs HEAD:");
    for f in repo.diff_working_tree()? {
        let (mut add, mut del) = (0, 0);
        for l in f.hunks.iter().flat_map(|h| &h.lines) {
            match l.kind {
                LineKind::Added => add += 1,
                LineKind::Removed => del += 1,
                LineKind::Context => {}
            }
        }
        let tag = match f.change {
            FileChange::Added => 'A',
            FileChange::Modified => 'M',
            FileChange::Deleted => 'D',
            FileChange::Renamed => 'R',
        };
        let note = if f.binary { " (binary)" } else { "" };
        println!("  {tag} {} +{add} -{del} in {} hunk(s){note}", f.path.display(), f.hunks.len());
    }
    println!("\nworktrees:");
    for w in repo.list_task_worktrees()? {
        println!("  {} -> {}", w.task, w.path.display());
    }
    Ok(())
}
