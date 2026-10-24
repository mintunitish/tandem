use std::fs;
use std::path::{Path, PathBuf};
use tandem_core::{FileChange, LineKind, TaskId};
use tandem_git::{Change, GitError, GutterKind, Repo, apply_file, apply_hunk_to_file, copy_file_from_worktree};
use tempfile::TempDir;

fn init() -> (TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    let r = git2::Repository::init(dir.path()).unwrap();
    let mut cfg = r.config().unwrap();
    cfg.set_str("user.name", "Test").unwrap();
    cfg.set_str("user.email", "t@example.com").unwrap();
    drop(cfg);
    drop(r);
    let repo = Repo::open(dir.path()).unwrap();
    (dir, repo)
}

fn write(root: &Path, rel: &str, s: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn base_repo() -> (TempDir, Repo, String) {
    let (dir, repo) = init();
    write(repo.root(), "a.txt", &numbered(40));
    write(repo.root(), "keep.txt", "keep\n");
    write(repo.root(), ".gitignore", "target/\n*.log\n");
    repo.stage(&["a.txt", "keep.txt", ".gitignore"]).unwrap();
    let id = repo.commit("initial").unwrap();
    (dir, repo, id)
}

#[test]
fn init_status_commit_log() {
    let (_d, repo) = init();
    assert!(repo.current_branch().unwrap().is_some());
    assert!(repo.head_commit().unwrap().is_none());
    assert!(repo.log(5).unwrap().is_empty());
    write(repo.root(), "x.txt", "x\n");
    let st = repo.status().unwrap();
    assert_eq!(st.len(), 1);
    assert_eq!((st[0].index, st[0].worktree), (Change::Unmodified, Change::Untracked));
    repo.stage(&["x.txt"]).unwrap();
    let st = repo.status().unwrap();
    assert_eq!((st[0].index, st[0].worktree), (Change::Added, Change::Unmodified));
    repo.unstage(&["x.txt"]).unwrap();
    assert_eq!(repo.status().unwrap()[0].worktree, Change::Untracked);
    repo.stage(&["x.txt"]).unwrap();
    assert!(repo.commit("add x").is_ok());
    assert!(matches!(repo.commit("again"), Err(GitError::Invalid(_))));
    let log = repo.log(10).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].summary, "add x");
    assert_eq!(log[0].author, "Test");
    assert_eq!(repo.head_commit().unwrap().unwrap().id, log[0].id);
    assert!(repo.status().unwrap().is_empty());
    // discovery from a subdirectory
    fs::create_dir_all(repo.root().join("sub/dir")).unwrap();
    let again = Repo::open(repo.root().join("sub/dir")).unwrap();
    assert_eq!(again.root(), repo.root());
}

#[test]
fn branches_and_checkout() {
    let (_d, repo, _) = base_repo();
    let main = repo.current_branch().unwrap().unwrap();
    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    assert_eq!(repo.current_branch().unwrap().unwrap(), "feature");
    let bs = repo.branches().unwrap();
    assert_eq!(bs.len(), 2);
    assert!(bs.iter().find(|b| b.name == "feature").unwrap().is_head);
    repo.checkout_branch(&main).unwrap();
    assert!(!repo.branches().unwrap().iter().find(|b| b.name == "feature").unwrap().is_head);
}

#[test]
fn working_tree_diff_gutter_blame() {
    let (_d, repo, _) = base_repo();
    let mut s = numbered(40);
    s = s.replace("line 5\n", "LINE five\n").replace("line 20\n", "");
    s.push_str("tail\n");
    write(repo.root(), "a.txt", &s);
    write(repo.root(), "new.txt", "n1\nn2\n");
    write(repo.root(), "debug.log", "ignored\n");
    write(repo.root(), "bin.dat", "ab\0cd");
    let files = repo.diff_working_tree().unwrap();
    let names: Vec<_> = files.iter().map(|f| f.path.to_str().unwrap()).collect();
    assert_eq!(names, ["a.txt", "bin.dat", "new.txt"]);
    let a = &files[0];
    assert_eq!(a.change, FileChange::Modified);
    assert_eq!(a.hunks.len(), 3);
    assert_eq!(a.old_text.as_deref(), Some(numbered(40).as_str()));
    let h0 = &a.hunks[0];
    assert_eq!((h0.old_start, h0.old_lines), (2, 7));
    assert!(h0.lines.iter().any(|l| l.kind == LineKind::Added && l.new_no == Some(5)));
    assert!(files[1].binary && files[1].new_text.is_none() && files[1].hunks.is_empty());
    assert_eq!(files[2].change, FileChange::Added);
    assert_eq!(files[2].hunks[0].new_lines, 2);

    let marks = repo.per_line_gutter_marks("a.txt").unwrap();
    assert!(marks.iter().any(|m| m.line == 5 && m.kind == GutterKind::Modified));
    assert!(marks.iter().any(|m| m.line == 20 && m.kind == GutterKind::Removed));
    assert!(marks.iter().any(|m| m.line == 40 && m.kind == GutterKind::Added));
    assert!(!marks.iter().any(|m| m.line == 10));
    let m = repo.per_line_gutter_marks("new.txt").unwrap();
    assert_eq!(m.len(), 2);
    assert!(repo.per_line_gutter_marks("bin.dat").unwrap().is_empty());

    let blame = repo.blame("keep.txt").unwrap();
    assert_eq!(blame.len(), 1);
    assert_eq!(blame[0].lines, 1..=1);
    assert_eq!(blame[0].summary, "initial");
}

#[test]
fn diff_commit_and_large_files() {
    let (_d, repo, id) = base_repo();
    let fs_ = repo.diff_commit(&id).unwrap();
    assert_eq!(fs_.len(), 3);
    assert!(fs_.iter().all(|f| f.change == FileChange::Added));
    write(repo.root(), "big.txt", &"x\n".repeat(1_100_000)); // 2.2 MB
    let d = repo.diff_working_tree().unwrap();
    let big = d.iter().find(|f| f.path == Path::new("big.txt")).unwrap();
    assert!(!big.binary && big.new_text.is_none() && big.hunks.is_empty());
}

fn task_worktree(repo: &Repo, id: &str, base: &str) -> (TaskId, PathBuf) {
    let task = TaskId::from(id);
    let p = repo.create_task_worktree(&task, Some(base)).unwrap();
    (task, p)
}

#[test]
fn worktree_lifecycle_and_diff() {
    let (_d, repo, base) = base_repo();
    let (task, wt) = task_worktree(&repo, "t1", &base);
    assert_eq!(wt, repo.root().join(".tandem/worktrees/t1"));
    assert!(wt.join("a.txt").exists());
    let ex = fs::read_to_string(repo.root().join(".git/info/exclude")).unwrap();
    assert!(ex.lines().any(|l| l == ".tandem/"));
    // .tandem must not show up as untracked in the main checkout
    assert!(repo.status().unwrap().is_empty());
    assert!(repo.current_branch().unwrap().is_some());
    let list = repo.list_task_worktrees().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].task, task);
    assert_eq!(list[0].branch.as_deref(), Some("tandem/t1"));
    assert_eq!(list[0].head.as_deref(), Some(base.as_str()));

    // agent edits: modify, add (untracked), ignored, binary, delete
    write(&wt, "a.txt", &numbered(40).replace("line 3\n", "three\n"));
    write(&wt, "src/new.rs", "fn main() {}\n");
    write(&wt, "noise.log", "ignored\n");
    write(&wt, "img.bin", "\0\x01\x02");
    fs::remove_file(wt.join("keep.txt")).unwrap();
    let ds = repo.diff_worktree_vs_base(&wt, &base).unwrap();
    assert_eq!(ds.task, task);
    assert_eq!(ds.base_commit, base);
    let by = |n: &str| ds.files.iter().find(|f| f.path == Path::new(n));
    assert_eq!(by("a.txt").unwrap().change, FileChange::Modified);
    let new = by("src/new.rs").expect("untracked file appears");
    assert_eq!(new.change, FileChange::Added);
    assert_eq!(new.new_text.as_deref(), Some("fn main() {}\n"));
    assert!(by("noise.log").is_none(), "gitignored file excluded");
    assert!(by("img.bin").unwrap().binary);
    assert_eq!(by("keep.txt").unwrap().change, FileChange::Deleted);
    assert_eq!(ds.files.len(), 4);

    // dirty worktree refuses non-force removal
    assert!(repo.remove_task_worktree(&task, false).is_err());
    assert!(wt.exists());
    repo.remove_task_worktree(&task, true).unwrap();
    assert!(!wt.exists());
    assert!(repo.list_task_worktrees().unwrap().is_empty());
    assert!(repo.branches().unwrap().iter().all(|b| b.name != "tandem/t1"));
    // clean worktree removes without force and keeps branch
    let (t2, wt2) = task_worktree(&repo, "t2", &base);
    repo.remove_task_worktree(&t2, false).unwrap();
    assert!(!wt2.exists());
    assert!(repo.branches().unwrap().iter().any(|b| b.name == "tandem/t2"));
    assert!(repo.create_task_worktree(&TaskId::from("../evil"), None).is_err());
}

#[test]
fn hunk_apply_round_trip() {
    let (_d, repo, base) = base_repo();
    let (task, wt) = task_worktree(&repo, "t3", &base);
    let edited = numbered(40)
        .replace("line 2\n", "line 2\nextra a\nextra b\n")
        .replace("line 18\n", "")
        .replace("line 19\n", "NINETEEN\n")
        .replace("line 38\n", "line 38\nline 38b\n");
    write(&wt, "a.txt", &edited);
    let ds = repo.diff_task(&task, &"claude".into(), &base).unwrap();
    assert_eq!(ds.agent.as_str(), "claude");
    let fd = &ds.files[0];
    assert!(fd.hunks.len() >= 3, "got {}", fd.hunks.len());
    for i in 0..fd.hunks.len() {
        apply_hunk_to_file(repo.root(), fd, i).unwrap();
    }
    assert_eq!(fs::read_to_string(repo.root().join("a.txt")).unwrap(), edited);
    // re-applying must fail (context no longer matches)
    assert!(matches!(apply_hunk_to_file(repo.root(), fd, 0), Err(GitError::Conflict { .. })));

    // applying a subset only touches that hunk; main-side edits elsewhere survive
    fs::write(repo.root().join("a.txt"), numbered(40).replace("line 30\n", "mine\n")).unwrap();
    apply_hunk_to_file(repo.root(), fd, 2).unwrap();
    let got = fs::read_to_string(repo.root().join("a.txt")).unwrap();
    assert!(got.contains("mine\n") && got.contains("line 38b\n") && !got.contains("NINETEEN"));

    // mismatch -> error and file untouched
    fs::write(repo.root().join("a.txt"), "totally different\n").unwrap();
    assert!(apply_hunk_to_file(repo.root(), fd, 1).is_err());
    assert_eq!(fs::read_to_string(repo.root().join("a.txt")).unwrap(), "totally different\n");
    assert!(apply_hunk_to_file(repo.root(), fd, 99).is_err());
}

#[test]
fn whole_file_apply_add_delete_binary() {
    let (_d, repo, base) = base_repo();
    let (task, wt) = task_worktree(&repo, "t4", &base);
    write(&wt, "a.txt", "replaced\n");
    write(&wt, "dir/added.txt", "hello\n");
    write(&wt, "blob.bin", "\0\0\0");
    fs::remove_file(wt.join("keep.txt")).unwrap();
    let ds = repo.diff_task(&task, &"codex".into(), &base).unwrap();
    for f in &ds.files {
        if f.binary {
            assert!(apply_file(repo.root(), f).is_err());
            copy_file_from_worktree(&wt, repo.root(), f).unwrap();
        } else {
            apply_file(repo.root(), f).unwrap();
        }
    }
    assert_eq!(fs::read_to_string(repo.root().join("a.txt")).unwrap(), "replaced\n");
    assert_eq!(fs::read_to_string(repo.root().join("dir/added.txt")).unwrap(), "hello\n");
    assert_eq!(fs::read(repo.root().join("blob.bin")).unwrap(), b"\0\0\0");
    assert!(!repo.root().join("keep.txt").exists());

    // strict: modified-on-main file is a conflict
    let a = ds.files.iter().find(|f| f.path == Path::new("a.txt")).unwrap();
    fs::write(repo.root().join("a.txt"), "someone else\n").unwrap();
    assert!(matches!(apply_file(repo.root(), a), Err(GitError::Conflict { .. })));
    // unsafe paths rejected
    let mut evil = a.clone();
    evil.path = PathBuf::from("../escape.txt");
    assert!(apply_file(repo.root(), &evil).is_err());
}

#[test]
fn added_file_hunk_apply() {
    let (_d, repo, base) = base_repo();
    let (task, wt) = task_worktree(&repo, "t5", &base);
    write(&wt, "n/new.txt", "a\nb\n");
    let ds = repo.diff_task(&task, &"x".into(), &base).unwrap();
    apply_hunk_to_file(repo.root(), &ds.files[0], 0).unwrap();
    assert_eq!(fs::read_to_string(repo.root().join("n/new.txt")).unwrap(), "a\nb\n");
}
