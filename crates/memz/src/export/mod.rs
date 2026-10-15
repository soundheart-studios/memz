mod html;
use crate::{
    Error, Result,
    auth::{Capability, Scope},
    capture::{digest, id, now},
    catalog::Catalog,
    state::StateRoot,
    store::NamespaceStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    pub namespace: String,
    pub revision: i64,
    pub jsonl: String,
    pub markdown: String,
    pub html: String,
}
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(Error::new("cancelled", "export cancelled"))
    } else {
        Ok(())
    }
}
fn escape(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars() {
        match c {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '!' | '|' => {
                result.push('\\');
                result.push(c);
            }
            _ => result.push(c),
        }
    }
    result
}
pub fn snapshot(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    max_bytes: usize,
    cancel: &AtomicBool,
) -> Result<Export> {
    cancelled(cancel)?;
    if max_bytes == 0 || max_bytes > 8388608 {
        return Err(Error::new(
            "export_limit",
            "export bound must be at most 8 MiB",
        ));
    }
    let _lock =
        rusqlite::Transaction::new_unchecked(&cat.conn, rusqlite::TransactionBehavior::Immediate)?;
    cat.authorize(cap, &Scope::project(&store.namespace), false)?;
    if store.state_path != cat.state.path {
        return Err(Error::new("denied", "access is not authorized"));
    }
    let tx = store.conn.transaction()?;
    let revision: i64 =
        tx.query_row("SELECT mutation_seq FROM namespace_meta", [], |r| r.get(0))?;
    let mut stmt=tx.prepare("SELECT i.id,i.revision,i.kind,i.content,i.explicit,i.status,i.source_id,i.source_revision,i.visibility,i.session_id,i.valid_from,i.valid_to,i.slot,s.origin,s.author,s.text,s.content_hash,s.event_at,s.ingested_at,s.retention,s.expires_at FROM items i JOIN sources s ON s.id=i.source_id AND s.revision=i.source_revision WHERE (i.visibility='project' OR (i.visibility='session' AND i.session_id=?1)) AND i.visibility=s.visibility AND i.session_id=s.session_id AND i.status<>'deleted' AND NOT EXISTS(SELECT 1 FROM sources d WHERE d.id=s.id AND d.status='deleted') AND (s.expires_at IS NULL OR s.expires_at>?2) AND NOT EXISTS(SELECT 1 FROM evidence e JOIN sources es ON es.id=e.source_id AND es.revision=e.source_revision WHERE e.item_id=i.id AND e.item_revision=i.revision AND (es.visibility<>i.visibility OR es.session_id<>i.session_id OR EXISTS(SELECT 1 FROM sources d WHERE d.id=es.id AND d.status='deleted'))) ORDER BY i.id,i.revision LIMIT 10001")?;
    let mut rows = stmt.query(rusqlite::params![cap.session(), now()])?;
    let mut records = String::new();
    let mut items = Vec::new();
    let mut count = 0;
    let mut markdown = format!(
        "# Memory snapshot\n\nNamespace: {}\n\nRevision: {revision}\n\n",
        escape(&store.namespace)
    );
    while let Some(row) = rows.next()? {
        cancelled(cancel)?;
        count += 1;
        if count > 10000 {
            return Err(Error::new("export_limit", "snapshot exceeds record bound"));
        }
        let mut record = json!({"id":row.get::<_,String>(0)?,"revision":row.get::<_,i64>(1)?,"kind":row.get::<_,String>(2)?,"content":row.get::<_,String>(3)?,"explicit":row.get::<_,bool>(4)?,"status":row.get::<_,String>(5)?,"source_id":row.get::<_,String>(6)?,"source_revision":row.get::<_,i64>(7)?,"visibility":row.get::<_,String>(8)?,"session":row.get::<_,String>(9)?,"valid_from":row.get::<_,i64>(10)?,"valid_to":row.get::<_,Option<i64>>(11)?,"slot":row.get::<_,Option<String>>(12)?,"origin":row.get::<_,String>(13)?,"author":row.get::<_,String>(14)?,"source_text":row.get::<_,String>(15)?,"source_hash":row.get::<_,String>(16)?,"event_at":row.get::<_,Option<i64>>(17)?,"ingested_at":row.get::<_,i64>(18)?,"retention":row.get::<_,String>(19)?,"expires_at":row.get::<_,Option<i64>>(20)?});
        record["current"] = match crate::graph::item(
            &tx,
            cap.session(),
            &store.namespace,
            &crate::graph::Node {
                id: record["id"].as_str().unwrap().into(),
                revision: record["revision"].as_i64().unwrap(),
            },
        ) {
            Ok(_) => json!(true),
            Err(error) if error.code == "denied" => json!(false),
            Err(error) => return Err(error),
        };
        items.push(record.clone());
        records.push_str(&record.to_string());
        records.push('\n');
        markdown.push_str(&format!("## {} revision {}\n\nScope: {} / {}. Status: {}. Origin: {}. Author: {}. Source: {} revision {}.\n\n{}\n\n",escape(record["id"].as_str().unwrap()),record["revision"],escape(record["visibility"].as_str().unwrap()),escape(record["session"].as_str().unwrap()),escape(record["status"].as_str().unwrap()),escape(record["origin"].as_str().unwrap()),escape(record["author"].as_str().unwrap()),escape(record["source_id"].as_str().unwrap()),record["source_revision"],escape(record["content"].as_str().unwrap())));
        if records.len() + markdown.len() > max_bytes {
            return Err(Error::new("export_limit", "snapshot exceeds byte bound"));
        }
    }
    drop(rows);
    drop(stmt);
    let nodes: std::collections::HashSet<_> = items
        .iter()
        .map(|v| {
            (
                v["id"].as_str().unwrap().to_string(),
                v["revision"].as_i64().unwrap(),
            )
        })
        .collect();
    let mut edges = Vec::new();
    let mut graph_partial = false;
    let mut edge_query = tx.prepare("SELECT id FROM edges WHERE status='active' AND (visibility='project' OR session_id=?1) ORDER BY id LIMIT 513")?;
    let ids = edge_query
        .query_map([cap.session()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if ids.len() > 512 {
        graph_partial = true;
    }
    for id in ids.into_iter().take(512) {
        cancelled(cancel)?;
        if let Some(edge) = crate::graph::read_edge(&tx, cap.session(), &store.namespace, &id)? {
            if !nodes.contains(&(edge.from.id.clone(), edge.from.revision))
                || !nodes.contains(&(edge.to.id.clone(), edge.to.revision))
            {
                continue;
            }
            if edges.len() == 128 {
                graph_partial = true;
                break;
            }
            edges.push(edge);
        }
    }
    drop(edge_query);
    drop(tx);
    drop(_lock);
    // Render only from the captured data, after releasing SQLite and authorization locks.
    let header = json!({"format":"memz-jsonl-v1","namespace":store.namespace,"revision":revision,"principal":cap.principal(),"session":cap.session(),"records":count,"sha256":digest(records.as_bytes()),"relationships":edges,"graph_partial":graph_partial,"render_version":1});
    let jsonl = format!("{header}\n{records}");
    for edge in &edges {
        markdown.push_str(&format!("\nRelation: {} revision {} → {} → {} revision {}. Authority: {}. Evidence establishes provenance, not truth.\n",escape(&edge.from.id),edge.from.revision,escape(&edge.relation),escape(&edge.to.id),edge.to.revision,if edge.explicit {"explicit"} else {"inferred"}));
    }
    let html = html::render(
        &store.namespace,
        revision,
        &items,
        &edges,
        graph_partial,
        max_bytes,
        cancel,
    )?;
    if jsonl.len() + markdown.len() + html.len() > max_bytes {
        return Err(Error::new("export_limit", "snapshot exceeds byte bound"));
    }
    Ok(Export {
        namespace: store.namespace.clone(),
        revision,
        jsonl,
        markdown,
        html,
    })
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn staged(directory: &Path, bytes: &[u8], cancel: &AtomicBool) -> Result<Temporary> {
    let path = directory.join(format!(".export-{}", id()?));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let temporary = Temporary(path);
    for chunk in bytes.chunks(65536) {
        cancelled(cancel)?;
        file.write_all(chunk)?;
    }
    file.sync_all()?;
    Ok(temporary)
}
pub fn publish(
    state: &StateRoot,
    name: &str,
    export: &Export,
    cancel: &AtomicBool,
) -> Result<(PathBuf, PathBuf)> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::new(
            "invalid_filename",
            "export name must be letters, digits, hyphens or underscores",
        ));
    }
    if export.jsonl.len() + export.markdown.len() + export.html.len() > 8388608 {
        return Err(Error::new("export_limit", "snapshot exceeds byte bound"));
    }
    let state = StateRoot::open(&state.path)?;
    let directory = state.path.join("exports");
    cancelled(cancel)?;
    let json = staged(&directory, export.jsonl.as_bytes(), cancel)?;
    let md = staged(&directory, export.markdown.as_bytes(), cancel)?;
    let html = staged(&directory, export.html.as_bytes(), cancel)?;
    let json_path = directory.join(format!("{name}.jsonl"));
    let md_path = directory.join(format!("{name}.md"));
    cancelled(cancel)?;
    fs::hard_link(&json.0, &json_path)?;
    if let Err(error) = fs::hard_link(&md.0, &md_path) {
        fs::remove_file(&json_path)?;
        return Err(error.into());
    }
    let html_path = directory.join(format!("{name}.html"));
    if let Err(error) = fs::hard_link(&html.0, &html_path) {
        fs::remove_file(&json_path)?;
        fs::remove_file(&md_path)?;
        return Err(error.into());
    }
    std::fs::File::open(&directory)?.sync_all()?;
    Ok((json_path, md_path))
}
pub(crate) fn parse_manifest(text: &str) -> Result<Value> {
    let header = text
        .lines()
        .next()
        .ok_or_else(|| Error::new("invalid_manifest", "missing manifest"))?;
    serde_json::from_str(header).map_err(|_| Error::new("invalid_manifest", "invalid manifest"))
}
