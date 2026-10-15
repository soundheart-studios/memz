use super::cancelled;
use crate::{Error, Result, capture::digest, graph::Edge};
use serde_json::Value;
use std::{collections::BTreeSet, sync::atomic::AtomicBool};
fn escape(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, c| {
        out.push_str(match c {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '\"' => "&quot;",
            '\'' => "&#39;",
            _ => {
                out.push(c);
                return out;
            }
        });
        out
    })
}
fn anchor(kind: &str, id: &str, revision: i64) -> String {
    let safe = if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        id.into()
    } else {
        digest(id.as_bytes())
    };
    format!("{kind}-{safe}-{revision}")
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn number(v: &Value, key: &str) -> i64 {
    v[key].as_i64().unwrap_or(0)
}
fn timestamp(value: Option<i64>) -> String {
    let Some(seconds) = value else {
        return "open-ended".into();
    };
    let raw = seconds as libc::time_t;
    let mut result = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: gmtime_r writes one tm into the valid output pointer and retains neither pointer.
    if unsafe { libc::gmtime_r(&raw, result.as_mut_ptr()) }.is_null() {
        return "date unavailable".into();
    }
    let time = unsafe { result.assume_init() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        i64::from(time.tm_year) + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    )
}
fn scope_detail(item: &Value) -> String {
    if text(item, "session").is_empty() {
        "project-wide".into()
    } else {
        format!("session {}", escape(text(item, "session")))
    }
}
fn bound(output: &str, max_bytes: usize, cancel: &AtomicBool) -> Result<()> {
    cancelled(cancel)?;
    if output.len() > max_bytes {
        Err(Error::new(
            "export_limit",
            "HTML exceeds snapshot byte bound",
        ))
    } else {
        Ok(())
    }
}
pub(super) fn render(
    namespace: &str,
    revision: i64,
    items: &[Value],
    edges: &[Edge],
    partial: bool,
    max_bytes: usize,
    cancel: &AtomicBool,
) -> Result<String> {
    let mut html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><title>Memz · Memory snapshot</title><style>{}</style></head><body><header><div class=\"eyebrow\">Memz / local snapshot</div><h1>Your memory, with its sources.</h1><p class=\"meta\">Namespace {} · Snapshot revision {} · Render version 1</p><nav aria-label=\"Sections\"><a href=\"#memories\">Memories</a><a href=\"#relationships\">Relationships</a><a href=\"#sources\">Sources</a></nav><p>Use your browser’s Find command to search this page. This is a saved snapshot; later changes do not update it.</p></header><main><section id=\"memories\"><h2>Memories and history</h2>",
        include_str!("../../../../assets/viewer/style.css"),
        escape(namespace),
        revision
    );
    for item in items {
        let state = text(item, "status");
        let current = item["current"] == true;
        let authority = if item["explicit"] == true {
            "explicit"
        } else {
            "inferred"
        };
        let source = anchor(
            "source",
            text(item, "source_id"),
            number(item, "source_revision"),
        );
        html.push_str(&format!("<article class=\"card {}\" id=\"{}\"><h3>{}</h3><span class=\"badge\">{}</span><span class=\"badge\">{}</span><span class=\"badge\">{}</span><p class=\"meta\">Item {} · revision {} · {} · valid from {} to {}</p><pre>{}</pre><p class=\"meta\">Origin {} · author {} · <a href=\"#{}\">Source revision {}</a></p></article>",if current{""}else{"history"},anchor("item",text(item,"id"),number(item,"revision")),escape(text(item,"kind")),escape(&format!("{} · {state}",if current{"current"}else{"history"})),authority,escape(text(item,"visibility")),escape(text(item,"id")),number(item,"revision"),scope_detail(item),timestamp(item["valid_from"].as_i64()),timestamp(item["valid_to"].as_i64()),escape(text(item,"content")),escape(text(item,"origin")),escape(text(item,"author")),source,number(item,"source_revision")));
        bound(&html, max_bytes, cancel)?;
    }
    if items.is_empty() {
        html.push_str("<p>No visible memories in this snapshot.</p>");
    }
    html.push_str("</section><section id=\"relationships\"><h2>Relationships and conflicts</h2><p>Evidence establishes provenance, not semantic proof. Arrows retain the stored direction. Inferred proposals are not user confirmations.</p>");
    if partial {
        html.push_str("<p class=\"card conflict\">Partial neighborhood: the edge scan or relationship limit was reached.</p>");
    }
    for edge in edges {
        html.push_str(&format!("<article class=\"card {}\" id=\"{}\"><h3>{}{}</h3><p><a href=\"#{}\">{} @ {}</a> → {} → <a href=\"#{}\">{} @ {}</a></p><p class=\"meta\">{} · {} · edge {} revision {}</p>",if edge.relation=="contradicts"{"conflict"}else{""},anchor("edge",&edge.id,edge.revision),if edge.relation=="contradicts"{"Conflict: "}else{""},escape(&edge.relation),anchor("item",&edge.from.id,edge.from.revision),escape(&edge.from.id),edge.from.revision,escape(&edge.relation),anchor("item",&edge.to.id,edge.to.revision),escape(&edge.to.id),edge.to.revision,if edge.explicit{"explicit"}else{"inferred"},escape(&edge.visibility),escape(&edge.id),edge.revision));
        for (key, value) in &edge.qualifiers {
            html.push_str(&format!("<p>{}: {}</p>", escape(key), escape(value)));
        }
        for proof in &edge.evidence {
            html.push_str(&format!("<blockquote>{}</blockquote><p class=\"meta\">Evidence: <a href=\"#{}\">{} @ {}</a> · bytes {}–{} · origin {} · author {} · method {} version {}</p>",escape(&proof.quote),anchor("source",&proof.source,proof.revision),escape(&proof.source),proof.revision,proof.start,proof.end,escape(&proof.origin),escape(&proof.author),escape(&proof.method),escape(&proof.method_version)));
        }
        html.push_str("</article>");
        bound(&html, max_bytes, cancel)?;
    }
    if edges.is_empty() {
        html.push_str("<p>No current authorized relationships in this snapshot.</p>");
    }
    html.push_str("</section><section id=\"sources\"><h2>Sources</h2>");
    let mut seen = BTreeSet::new();
    for item in items {
        let id = anchor(
            "source",
            text(item, "source_id"),
            number(item, "source_revision"),
        );
        if !seen.insert(id.clone()) {
            continue;
        }
        html.push_str(&format!("<article class=\"card\" id=\"{}\"><h3>Source {} · revision {}</h3><p class=\"meta\">{} · {} · origin {} · author {} · SHA-256 {}</p><pre>{}</pre></article>",id,escape(text(item,"source_id")),number(item,"source_revision"),escape(text(item,"visibility")),scope_detail(item),escape(text(item,"origin")),escape(text(item,"author")),escape(text(item,"source_hash")),escape(text(item,"source_text"))));
        bound(&html, max_bytes, cancel)?;
    }
    // An edge can cite a source with no separately exported item. Show only its authorized exact span.
    for edge in edges {
        for proof in &edge.evidence {
            let id = anchor("source", &proof.source, proof.revision);
            if seen.insert(id.clone()) {
                html.push_str(&format!("<article class=\"card\" id=\"{}\"><h3>Evidence source {} · revision {}</h3><p class=\"meta\">Authorized excerpt, bytes {}–{}. Origin {} · author {}</p><blockquote>{}</blockquote></article>",id,escape(&proof.source),proof.revision,proof.start,proof.end,escape(&proof.origin),escape(&proof.author),escape(&proof.quote)));
                bound(&html, max_bytes, cancel)?;
            }
        }
    }
    html.push_str("</section></main><footer>Private export · Access was checked when the snapshot was created. Sharing this file shares its contents. Forgetting live memory cannot recall saved exports. No scripts, network requests or model runtime.</footer></body></html>");
    bound(&html, max_bytes, cancel)?;
    Ok(html)
}
