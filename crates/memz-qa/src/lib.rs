//! Deterministic project evidence checkers. Infrastructure errors never prove behavior.
pub mod qualification;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| format!("missing string: {k}"))
}
fn strings(v: &Value, k: &str) -> Result<Vec<String>> {
    v[k].as_array()
        .ok_or_else(|| format!("missing array: {k}"))?
        .iter()
        .map(|x| {
            x.as_str()
                .map(str::to_owned)
                .ok_or("expected string".into())
        })
        .collect()
}
fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn under(path: &str, prefix: &str) -> bool {
    path == prefix.trim_end_matches('/')
        || path.starts_with(&format!("{}/", prefix.trim_end_matches('/')))
}
fn safe_path(s: &str) -> Result<()> {
    if s.is_empty()
        || Path::new(s)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        Err(format!("unsafe relative path: {s}"))
    } else {
        Ok(())
    }
}
fn output(mut command: Command) -> Result<Output> {
    // File-backed capture avoids pipe deadlocks on verbose cargo output.
    let out = tempfile::tempfile().map_err(|e| e.to_string())?;
    let err = tempfile::tempfile().map_err(|e| e.to_string())?;
    command
        .stdout(Stdio::from(out.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(err.try_clone().map_err(|e| e.to_string())?));
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("checker subprocess timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    use std::io::{Read, Seek};
    let mut out = out;
    let mut err = err;
    out.rewind().map_err(|e| e.to_string())?;
    err.rewind().map_err(|e| e.to_string())?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    out.take(4 * 1024 * 1024)
        .read_to_end(&mut stdout)
        .map_err(|e| e.to_string())?;
    err.take(4 * 1024 * 1024)
        .read_to_end(&mut stderr)
        .map_err(|e| e.to_string())?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    let out = output(cmd)?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}
fn commits(repo: &Path, task: &str) -> Result<Vec<String>> {
    let log = git(repo, &["log", "--reverse", "--format=%H %s"])?;
    let found: Vec<_> = log
        .lines()
        .filter_map(|line| {
            let (sha, subject) = line.split_once(' ')?;
            subject
                .starts_with(&format!("{task}: "))
                .then(|| sha.to_string())
        })
        .collect();
    if found.is_empty() {
        return Err("no task-prefixed commits".into());
    }
    Ok(found)
}
fn landed(repo: &Path, commits: &[String]) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    for sha in commits {
        for p in git(
            repo,
            &[
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "-r",
                sha,
            ],
        )?
        .lines()
        {
            safe_path(p)?;
            paths.insert(p.to_string());
        }
    }
    Ok(paths.into_iter().collect())
}
pub fn classify_test(success: bool, text: &str) -> Result<bool> {
    if text.contains("error[E")
        || text.contains("could not compile")
        || text.contains("command not found")
    {
        return Err("test infrastructure/compilation failure".into());
    }
    let executed = text
        .lines()
        .any(|l| l.starts_with("test ") && (l.ends_with(" ... ok") || l.ends_with(" ... FAILED")));
    if !executed {
        return Err("no executed tests".into());
    }
    if success && text.contains("test result: ok.") {
        Ok(false)
    } else if !success
        && text.contains("test result: FAILED.")
        && text
            .lines()
            .any(|l| l.starts_with("test ") && l.ends_with(" ... FAILED"))
    {
        Ok(true)
    } else {
        Err("unrecognized test outcome".into())
    }
}
fn test(repo: &Path, reference: &str) -> Result<bool> {
    let args: Vec<_> = reference.split_whitespace().collect();
    if args.len() < 3
        || args[0] != "cargo"
        || args[1] != "test"
        || args
            .iter()
            .any(|a| a.chars().any(|c| ";&|`$<>".contains(c)))
    {
        return Err("only explicit cargo test refs supported".into());
    }
    let mut cmd = Command::new("cargo");
    cmd.args(&args[1..]).current_dir(repo);
    let run_id = std::env::var("PW_RUN_ID").unwrap_or_else(|_| {
        format!(
            "memzqa-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    });
    // An inherited CARGO_TARGET_DIR would let concurrent scratch clones overwrite
    // each other's build artifacts and corrupt mutation outcomes.
    cmd.env("PW_RUN_ID", run_id)
        .env("MEMZ_HOME", repo.join(".memz-fixture"))
        .env("CARGO_TARGET_DIR", repo.join("target"));
    let out = output(cmd)?;
    classify_test(
        out.status.success(),
        &format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
    .map_err(|e| {
        format!(
            "{e}: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}
pub fn run(kind: &str, input: &Value) -> Result<Value> {
    match kind {
        "schema-contract" => schema(input),
        "territory-check" => territory(input),
        "verify-evidence" => verify(input),
        "release-validation" => release(input),
        "qualify-performance" | "qualify-quality" => {
            qualification::run(kind, input).map_err(|e| e.to_string())
        }
        _ => Err(format!("unknown checker: {kind}")),
    }
}
fn territory(v: &Value) -> Result<Value> {
    let repo = Path::new(field(v, "repo")?);
    let task = field(v, "taskId")?;
    let declared = strings(v, "declared")?;
    if declared.is_empty() {
        return Err("no declaration".into());
    }
    let cs = commits(repo, task)?;
    let paths = landed(repo, &cs)?;
    let (covered, outside): (Vec<_>, Vec<_>) = paths
        .iter()
        .cloned()
        .partition(|p| declared.iter().any(|d| under(p, d)));
    Ok(
        json!({"taskId":task,"commits":cs.len(),"landed":paths,"covered":covered,"outside":outside,"ambiguous":[]}),
    )
}
fn schema(v: &Value) -> Result<Value> {
    let db = Path::new(field(v, "database")?);
    let manifest = read_json(Path::new(field(v, "manifest")?))?;
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
    let tables = manifest["tables"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("empty table contract")?;
    let mut checked = Vec::new();
    let mut violations = Vec::new();
    for table in tables {
        let name = field(table, "name")?;
        let mut stmt = conn
            .prepare("SELECT name,type,\"notnull\",pk FROM pragma_table_info(?1)")
            .map_err(|e| e.to_string())?;
        let actual: Vec<(String, String, bool, i64)> = stmt
            .query_map([name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| e.to_string())?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let columns = table["columns"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or("empty column contract")?;
        for col in columns {
            let cn = field(col, "name")?;
            let ty = field(col, "type")?;
            checked.push(format!("{name}.{cn}"));
            if !actual.iter().any(|(n, t, nn, pk)| {
                n == cn
                    && t.eq_ignore_ascii_case(ty)
                    && col["notnull"].as_bool().is_none_or(|x| x == *nn)
                    && col["pk"].as_i64().is_none_or(|x| x == *pk)
            }) {
                violations.push(format!("column mismatch: {name}.{cn}"));
            }
        }
        if let Some(indexes) = table["indexes"].as_array() {
            for index in indexes {
                let idx = index.as_str().ok_or("invalid index name")?;
                checked.push(format!("index:{idx}"));
                let count:i64=conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type='index' AND name=?1 AND tbl_name=?2",[idx,name],|r|r.get(0)).map_err(|e|e.to_string())?;
                if count != 1 {
                    violations.push(format!("missing index: {idx}"));
                }
            }
        }
        // Exact SQL fragments allow CHECK, UNIQUE, FK and index expression contracts.
        if let Some(fragments) = table["sql_contains"].as_array() {
            let sql: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
                    [name],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            for fragment in fragments {
                let s = fragment.as_str().ok_or("invalid SQL constraint")?;
                checked.push(format!("constraint:{name}:{s}"));
                if !sql.contains(s) {
                    violations.push(format!("missing constraint: {name}:{s}"));
                }
            }
        }
    }
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    checked.push("integrity".into());
    if integrity != "ok" {
        violations.push(integrity);
    }
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(|e| e.to_string())?;
    if stmt
        .query([])
        .map_err(|e| e.to_string())?
        .next()
        .map_err(|e| e.to_string())?
        .is_some()
    {
        violations.push("foreign key violation".into());
    }
    checked.push("foreign_keys".into());
    Ok(json!({"checked":checked,"violations":violations}))
}
fn verify(v: &Value) -> Result<Value> {
    let mode = v["mode"].as_str().unwrap_or("historical-revert");
    if !["historical-revert", "rust-function-bodies"].contains(&mode) {
        return Err("unsupported mutation mode".into());
    }
    let repo = Path::new(field(v, "repo")?);
    let task = field(v, "taskId")?;
    let refs = strings(v, "testRefs")?;
    let territory = strings(v, "territory")?;
    if refs.is_empty() {
        return Err("no test refs".into());
    }
    let criteria = v["criteria"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("no criteria")?;
    for c in criteria {
        let cr = strings(c, "refs")?;
        if cr.is_empty() || cr.iter().any(|r| !refs.contains(r)) {
            return Err("criterion has missing/unrun refs".into());
        }
    }
    let cs = commits(repo, task)?;
    let paths = landed(repo, &cs)?;
    let subjects: Vec<_> = paths
        .into_iter()
        .filter(|p| {
            territory.iter().any(|d| under(p, d))
                && !p.contains("/tests/")
                && !p.starts_with("tests/")
                && !p.ends_with("Cargo.toml")
                && !p.ends_with("Cargo.lock")
        })
        .collect();
    if subjects.is_empty() {
        return Err("nothing to revert".into());
    }
    // Clone committed history; never mutate the caller's checkout or state store.
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let scratch = temp.path().join("repo");
    let mut clone = Command::new("git");
    clone
        .args(["clone", "--quiet", "--no-hardlinks"])
        .arg(repo)
        .arg(&scratch);
    if !output(clone)?.status.success() {
        return Err("scratch clone failed".into());
    }
    for reference in &refs {
        if test(&scratch, reference)? {
            return Err("baseline tests are not green".into());
        }
    }
    let base = git(repo, &["rev-parse", &format!("{}^", cs[0])]).ok();
    for path in &subjects {
        safe_path(path)?;
        let destination = scratch.join(path);
        if mode == "rust-function-bodies" && path.ends_with(".rs") {
            let source = fs::read_to_string(&destination).map_err(|e| e.to_string())?;
            fs::write(&destination, remove_bodies(&source)?).map_err(|e| e.to_string())?;
            continue;
        }
        let old = base
            .as_ref()
            .and_then(|b| git(repo, &["show", &format!("{}:{path}", b.trim())]).ok());
        if let Some(text) = old {
            fs::write(&destination, text).map_err(|e| e.to_string())?;
        } else {
            fs::remove_file(&destination).map_err(|e| e.to_string())?;
        }
    }
    let mut failed = Vec::new();
    let mut survived = Vec::new();
    for reference in &refs {
        if test(&scratch, reference)? {
            failed.push(reference.clone())
        } else {
            survived.push(reference.clone())
        }
    }
    let mut outcomes = Vec::new();
    let mut declared = Vec::new();
    let mut proved = Vec::new();
    for criterion in criteria {
        let text = field(criterion, "text")?;
        let cr = strings(criterion, "refs")?;
        let yes = cr.iter().any(|r| failed.contains(r));
        declared.push(text);
        if yes {
            proved.push(text)
        }
        outcomes.push(json!({"text":text,"refs":cr,"outcome":if yes{"proved"}else{"unproved"}}));
    }
    Ok(
        json!({"taskId":task,"impl":subjects,"reverted":subjects,"refs":refs,"failed":failed,"survived":survived,"error":null,"criteria":outcomes,"declared":declared,"proved":proved,"mutationMode":mode}),
    )
}
fn release(v: &Value) -> Result<Value> {
    let repo = Path::new(field(v, "repo")?);
    let run_id = field(v, "runId")?;
    let manifest = read_json(Path::new(field(v, "manifest")?))?;
    let checks = manifest["checks"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("empty release checks")?;
    let artifacts = strings(&manifest, "artifacts")?;
    if artifacts.is_empty() {
        return Err("no release artifacts".into());
    }
    let mut required = BTreeSet::new();
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for check in checks {
        let id = field(check, "id")?;
        if !required.insert(id.to_owned()) {
            return Err("duplicate check".into());
        }
        let argv = strings(check, "argv")?;
        if argv.is_empty() {
            return Err("empty command".into());
        }
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(repo)
            .env("PW_RUN_ID", run_id);
        if output(cmd)?.status.success() {
            passed.push(id)
        } else {
            failed.push(id)
        }
    }
    for artifact in &artifacts {
        safe_path(artifact)?;
        let path = repo.join(artifact);
        if !path.is_file() || fs::metadata(path).map_err(|e| e.to_string())?.len() == 0 {
            return Err(format!("missing/empty artifact: {artifact}"));
        }
    }
    Ok(json!({"required":required,"passed":passed,"failed":failed,"artifacts":artifacts}))
}

fn remove_bodies(source: &str) -> Result<String> {
    use syn::visit_mut::VisitMut;
    struct Remove {
        count: usize,
    }
    impl VisitMut for Remove {
        fn visit_item_fn_mut(&mut self, node: &mut syn::ItemFn) {
            if node.attrs.iter().any(|a| a.path().is_ident("test")) {
                return;
            }
            *node.block =
                syn::parse_quote!({ panic!("implementation removed by evidence verifier") });
            self.count += 1;
        }
        fn visit_impl_item_fn_mut(&mut self, node: &mut syn::ImplItemFn) {
            node.block =
                syn::parse_quote!({ panic!("implementation removed by evidence verifier") });
            self.count += 1;
        }
        fn visit_item_mod_mut(&mut self, node: &mut syn::ItemMod) {
            if node.attrs.iter().any(|a|matches!(&a.meta,syn::Meta::List(l) if l.path.is_ident("cfg") && l.tokens.to_string() == "test")) { return; }
            syn::visit_mut::visit_item_mod_mut(self, node);
        }
    }
    let mut syntax = syn::parse_file(source).map_err(|e| e.to_string())?;
    let mut visitor = Remove { count: 0 };
    visitor.visit_file_mut(&mut syntax);
    if visitor.count == 0 {
        return Err("no executable bodies to remove".into());
    }
    Ok(prettyplease::unparse(&syntax))
}
