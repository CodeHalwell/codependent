//! Outcome 14 — the live code graph: an edit made DURING a session, with no
//! commit and no restart, is visible to the next query.
//!
//! The 2026-08-13 review disproved this by appending an uncommitted symbol to a
//! scanned repository, launching two more runs, and finding the node count
//! unmoved at 345 with the new symbol absent: the graph's only refresh trigger
//! was a git `HEAD` change, and `HEAD` does not move when a file is edited. This
//! reproduces that experiment against the armed watcher and asserts the opposite
//! result, using the SAME production functions the daemon calls —
//! [`scan::scan_repository`], [`scan::arm_watcher`], and
//! `codegraph::answer` — never a test-local reimplementation.
//!
//! Counts are printed, not just asserted, so a reader can see the graph move.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use codypendent_codypendentd::scan;
use codypendent_knowledge::codegraph::{self, GraphQuestion};
use codypendent_knowledge::db;
use codypendent_protocol::RepositoryId;
use sqlx::SqlitePool;

/// How long a poll waits for the debounced watcher to fold a change. Generous
/// against `WATCH_DEBOUNCE` (400 ms) so a loaded CI box does not flake; the
/// assertions below fail on timeout, never pass silently.
const SETTLE: Duration = Duration::from_secs(20);

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A committed scratch repository with two source files, mirroring the review's
/// probe: a router with a `decide` method its own test calls.
fn seed_repository(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/router.rs"),
        "pub struct Router;\n\
         \n\
         impl Router {\n\
         \x20   pub fn decide(&self) -> u32 { classify() }\n\
         }\n\
         \n\
         pub fn classify() -> u32 { 1 }\n\
         \n\
         #[cfg(test)]\n\
         mod tests {\n\
         \x20   use super::*;\n\
         \x20   #[test]\n\
         \x20   fn routes() { let _ = Router.decide(); }\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/doomed.rs"),
        "pub fn about_to_be_deleted() -> u32 { 0 }\n",
    )
    .unwrap();
    // A generated file the ignore policy must keep out of the graph.
    std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    std::fs::create_dir_all(root.join("generated")).unwrap();
    std::fs::write(
        root.join("generated/machine.rs"),
        "pub fn machine_written() -> u32 { 0 }\n",
    )
    .unwrap();

    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.email", "probe@example.invalid"]);
    git(root, &["config", "user.name", "probe"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "seed"]);
}

async fn counts(pool: &SqlitePool, repository: RepositoryId) -> (usize, usize) {
    (
        codegraph::nodes(pool, repository).await.unwrap().len(),
        codegraph::edges(pool, repository).await.unwrap().len(),
    )
}

async fn has_symbol(pool: &SqlitePool, repository: RepositoryId, name: &str) -> bool {
    !codegraph::find_symbols(pool, repository, name, 5)
        .await
        .unwrap()
        .is_empty()
}

/// Poll until `check` holds or [`SETTLE`] elapses. Returns whether it held.
async fn settle<F, Fut>(mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + SETTLE;
    loop {
        if check().await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uncommitted_edit_reaches_the_graph_without_a_commit_or_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    seed_repository(&root);

    let data = tempfile::tempdir().unwrap();
    let pool = db::open(&data.path().join("codypendent.db")).await.unwrap();
    let repository = scan::repository_id_for(&root);

    // 1. The warm-up scan, exactly as the daemon runs it.
    {
        let _guard = scan::lock_repository(repository).await;
        scan::scan_repository(&pool, repository, &root)
            .await
            .unwrap();
    }
    let before = counts(&pool, repository).await;
    println!("after scan:          nodes={} edges={}", before.0, before.1);
    assert!(has_symbol(&pool, repository, "Router::decide").await);
    assert!(
        !has_symbol(&pool, repository, "machine_written").await,
        "a .gitignore'd generated file is not in the graph"
    );

    // 2. Arm the watcher — the wire outcome 14 needs.
    let _watcher = scan::arm_watcher(pool.clone(), repository, &root).expect("arm the watcher");

    // 3. THE REVIEW'S EXPERIMENT: rename a method and add a symbol, and do NOT
    //    commit. `HEAD` does not move, so the old revision gate stays shut.
    let head_before = Command::new("git")
        .current_dir(&root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    std::fs::write(
        root.join("src/router.rs"),
        "pub struct Router;\n\
         \n\
         impl Router {\n\
         \x20   pub fn choose(&self) -> u32 { classify() }\n\
         }\n\
         \n\
         pub fn classify() -> u32 { 1 }\n\
         \n\
         pub fn uncommitted_symbol_plugh() -> u32 { 7 }\n\
         \n\
         #[cfg(test)]\n\
         mod tests {\n\
         \x20   use super::*;\n\
         \x20   #[test]\n\
         \x20   fn routes() { let _ = Router.choose(); }\n\
         }\n",
    )
    .unwrap();

    assert!(
        settle(|| async { has_symbol(&pool, repository, "uncommitted_symbol_plugh").await }).await,
        "the watcher never folded the uncommitted edit"
    );

    let after = counts(&pool, repository).await;
    println!(
        "after uncommitted edit: nodes={} edges={}",
        after.0, after.1
    );
    let head_after = Command::new("git")
        .current_dir(&root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        head_before.stdout, head_after.stdout,
        "HEAD did not move — the old revision gate would never have opened"
    );

    // 4. The next query reflects the edit. This is the user-visible consequence
    //    the review named: "user asks the agent to refactor `Router::decide`
    //    into `Router::choose`; every later turn's repository map still says
    //    `decide`."
    assert!(
        !has_symbol(&pool, repository, "Router::decide").await,
        "the renamed-away symbol was retired"
    );
    let answer = codegraph::answer(
        &pool,
        repository,
        &GraphQuestion::CallersOf {
            symbol: "classify".to_owned(),
        },
    )
    .await
    .unwrap();
    let rendered = answer.render();
    println!("graph.callers_of classify →\n{rendered}");
    assert!(
        rendered.contains("Router::choose"),
        "the query answers from the edited tree:\n{rendered}"
    );
    assert!(
        !rendered.contains("Router::decide"),
        "the pre-edit symbol is gone:\n{rendered}"
    );
    assert!(
        rendered.contains("+workdir"),
        "an uncommitted fold says so in its revision:\n{rendered}"
    );

    // 5. A deleted file's symbols are retired too — the half a reparse can never
    //    do, because nothing reparses a file that no longer exists.
    assert!(has_symbol(&pool, repository, "about_to_be_deleted").await);
    std::fs::remove_file(root.join("src/doomed.rs")).unwrap();
    assert!(
        settle(|| async { !has_symbol(&pool, repository, "about_to_be_deleted").await }).await,
        "a deleted file's symbols were never retired"
    );
    let final_counts = counts(&pool, repository).await;
    println!(
        "after delete:        nodes={} edges={}",
        final_counts.0, final_counts.1
    );
    assert!(
        final_counts.0 < after.0,
        "deleting a file shrinks the graph: {} → {}",
        after.0,
        final_counts.0
    );

    // 6. The ignore policy still holds for a live write, not just for the walk.
    std::fs::write(
        root.join("generated/machine.rs"),
        "pub fn machine_written_v2() -> u32 { 1 }\n",
    )
    .unwrap();
    tokio::time::sleep(scan::WATCH_DEBOUNCE * 4).await;
    assert!(
        !has_symbol(&pool, repository, "machine_written_v2").await,
        "a .gitignore'd file must not enter the graph through the watcher either"
    );
}

/// F6: the two triggers `codypendent run` fires back to back (`CreateSession`
/// then `StartRun`) must not both clear-and-rebuild the same repository.
///
/// Before [`scan::lock_repository`], both observed "not folded" and raced —
/// reproducibly `database is locked`, the revision guard never recorded, and a
/// torn graph readable in between. Here two concurrent warm-ups run the real
/// production sequence; both must succeed and the graph must be whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_warm_ups_do_not_race_the_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    seed_repository(&root);

    let data = tempfile::tempdir().unwrap();
    let pool = db::open(&data.path().join("codypendent.db")).await.unwrap();
    let repository = scan::repository_id_for(&root);

    // A serial baseline: what a single, unraced scan produces.
    {
        let _guard = scan::lock_repository(repository).await;
        scan::scan_repository(&pool, repository, &root)
            .await
            .unwrap();
    }
    let baseline = counts(&pool, repository).await;
    println!("baseline: nodes={} edges={}", baseline.0, baseline.1);
    assert!(baseline.0 > 0);

    // Now the race: four warm-ups launched together, each taking the same gate
    // the executor takes.
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let pool = pool.clone();
        let root = root.clone();
        tasks.push(tokio::spawn(async move {
            let _guard = scan::lock_repository(repository).await;
            scan::scan_repository(&pool, repository, &root).await
        }));
    }
    for task in tasks {
        task.await
            .unwrap()
            .expect("every concurrent warm-up succeeds");
    }

    let raced = counts(&pool, repository).await;
    println!(
        "after 4 concurrent warm-ups: nodes={} edges={}",
        raced.0, raced.1
    );
    assert_eq!(
        raced, baseline,
        "a raced rebuild yields the same graph as a serial one"
    );
}
