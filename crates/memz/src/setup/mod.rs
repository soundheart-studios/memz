//! Trusted host setup: connects agent harnesses in a project to a Memz client.
//!
//! `memz init` calls [`configure_project`] after issuing a client. It merges a
//! `memz` MCP server into Claude Code's project `.mcp.json` (pre-approved in
//! `.claude/settings.local.json`) and Codex's project `.codex/config.toml`,
//! installs the memory-policy skill for both, and keeps machine-specific files
//! out of git through `.git/info/exclude` when they are not already tracked.
use crate::{Error, Result};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const SKILL: &str = include_str!("../../../../.claude/skills/memz/SKILL.md");
const TRIGGERS: &str = include_str!("../../../../.claude/skills/memz/triggers.md");
const CODEX_BEGIN: &str =
    "# >>> memz: managed by `memz init`; edits inside this block are overwritten";
const CODEX_END: &str = "# <<< memz";

/// What the server entry launches.
pub struct Server<'a> {
    pub executable: &'a Path,
    pub client_file: &'a Path,
    /// Non-default state directory the server must be told about, if any.
    pub memz_home: Option<&'a Path>,
}

/// One file the setup created, updated or deliberately left alone.
#[derive(Debug, serde::Serialize)]
pub struct Change {
    pub path: PathBuf,
    pub action: &'static str,
}

pub fn configure_project(root: &Path, server: &Server) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    let mut machine_specific = Vec::new();

    let mcp = root.join(".mcp.json");
    changes.push(merge_json(&mcp, |v| {
        object(v)?.entry("mcpServers").or_insert_with(|| json!({}));
        object(&mut v["mcpServers"])?.insert("memz".into(), claude_entry(server)?);
        Ok(())
    })?);
    machine_specific.push(".mcp.json");

    let settings = root.join(".claude").join("settings.local.json");
    changes.push(merge_json(&settings, |v| {
        let list = object(v)?
            .entry("enabledMcpjsonServers")
            .or_insert_with(|| json!([]));
        let list = list.as_array_mut().ok_or_else(|| {
            Error::new(
                "setup",
                "enabledMcpjsonServers in settings.local.json is not an array",
            )
        })?;
        if !list.iter().any(|s| s == "memz") {
            list.push(json!("memz"));
        }
        Ok(())
    })?);
    machine_specific.push(".claude/settings.local.json");

    changes.push(write_codex(
        &root.join(".codex").join("config.toml"),
        server,
    )?);
    machine_specific.push(".codex/config.toml");

    for base in [".claude/skills/memz", ".agents/skills/memz"] {
        for (name, body) in [("SKILL.md", SKILL), ("triggers.md", TRIGGERS)] {
            changes.push(write_if_absent(&root.join(base).join(name), body)?);
        }
    }

    changes.extend(exclude_untracked(root, &machine_specific)?);
    Ok(changes)
}

fn claude_entry(server: &Server) -> Result<Value> {
    let mut entry = json!({
        "type": "stdio",
        "command": utf8(server.executable)?,
        "args": ["mcp", "--client", utf8(server.client_file)?],
    });
    if let Some(home) = server.memz_home {
        entry["env"] = json!({ "MEMZ_HOME": utf8(home)? });
    }
    Ok(entry)
}

fn codex_block(server: &Server) -> Result<String> {
    let mut block = format!(
        "{CODEX_BEGIN}\n[mcp_servers.memz]\ncommand = {}\nargs = [\"mcp\", \"--client\", {}]\n",
        toml_string(utf8(server.executable)?),
        toml_string(utf8(server.client_file)?),
    );
    if let Some(home) = server.memz_home {
        block.push_str(&format!(
            "env = {{ MEMZ_HOME = {} }}\n",
            toml_string(utf8(home)?)
        ));
    }
    block.push_str(CODEX_END);
    block.push('\n');
    Ok(block)
}

/// Replaces the managed block, or appends one. A hand-written
/// `[mcp_servers.memz]` table outside the block is left alone and reported,
/// because a second definition would make the file invalid TOML.
fn write_codex(path: &Path, server: &Server) -> Result<Change> {
    let block = codex_block(server)?;
    let existing = read_optional(path)?;
    let text = existing.clone().unwrap_or_default();
    let updated = match (text.find(CODEX_BEGIN), text.find(CODEX_END)) {
        (Some(start), Some(end)) if end > start => {
            let after = text[end..].find('\n').map_or(text.len(), |n| end + n + 1);
            format!("{}{}{}", &text[..start], block, &text[after..])
        }
        _ if text.lines().any(|l| l.trim() == "[mcp_servers.memz]") => {
            return Ok(Change {
                path: path.into(),
                action: "skipped: unmanaged [mcp_servers.memz] already present",
            });
        }
        _ if text.is_empty() => block,
        _ => {
            let sep = if text.ends_with('\n') { "\n" } else { "\n\n" };
            format!("{text}{sep}{block}")
        }
    };
    write_changed(path, existing.as_deref(), &updated)
}

fn merge_json(path: &Path, edit: impl FnOnce(&mut Value) -> Result<()>) -> Result<Change> {
    let existing = read_optional(path)?;
    let mut value = match existing.as_deref() {
        None => json!({}),
        Some(text) if text.trim().is_empty() => json!({}),
        Some(text) => serde_json::from_str(text).map_err(|_| {
            Error::new(
                "setup",
                format!(
                    "{} is not valid JSON; fix or remove it, then rerun init",
                    path.display()
                ),
            )
        })?,
    };
    edit(&mut value)?;
    let mut text = serde_json::to_string_pretty(&value)
        .map_err(|_| Error::new("setup", "settings encoding failed"))?;
    text.push('\n');
    write_changed(path, existing.as_deref(), &text)
}

fn object(v: &mut Value) -> Result<&mut serde_json::Map<String, Value>> {
    v.as_object_mut()
        .ok_or_else(|| Error::new("setup", "agent configuration root is not a JSON object"))
}

fn write_if_absent(path: &Path, body: &str) -> Result<Change> {
    match read_optional(path)? {
        Some(current) if current == body => Ok(Change {
            path: path.into(),
            action: "unchanged",
        }),
        Some(_) => Ok(Change {
            path: path.into(),
            action: "kept: existing file differs",
        }),
        None => write_changed(path, None, body),
    }
}

fn write_changed(path: &Path, existing: Option<&str>, text: &str) -> Result<Change> {
    let action = match existing {
        Some(current) if current == text => "unchanged",
        Some(_) => "updated",
        None => "created",
    };
    if action != "unchanged" {
        let previous = fs::symlink_metadata(path).ok();
        if previous
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink())
        {
            return Ok(Change {
                path: path.into(),
                action: "skipped: symlink; update it by hand",
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write a sibling then rename, so an interrupted write never truncates
        // a user's existing agent configuration.
        let staging = path.with_extension(format!(
            "{}memz-tmp",
            path.extension()
                .map(|e| format!("{}.", e.to_string_lossy()))
                .unwrap_or_default()
        ));
        let mut file = fs::File::create(&staging)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        if let Some(meta) = previous {
            fs::set_permissions(&staging, meta.permissions())?;
        }
        fs::rename(&staging, path)?;
    }
    Ok(Change {
        path: path.into(),
        action,
    })
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Adds untracked machine-specific files to the repository's local exclude
/// list. Tracked files are reported instead of silently ignored.
fn exclude_untracked(root: &Path, relative: &[&str]) -> Result<Vec<Change>> {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(root)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
    };
    let Some(out) =
        git(&["rev-parse", "--git-path", "info/exclude"]).filter(|o| o.status.success())
    else {
        return Ok(Vec::new());
    };
    let exclude = root.join(String::from_utf8_lossy(&out.stdout).trim());
    let mut text = read_optional(&exclude)?.unwrap_or_default();
    let original = text.clone();
    let mut changes = Vec::new();
    for path in relative {
        let tracked =
            git(&["ls-files", "--error-unmatch", "--", path]).is_some_and(|o| o.status.success());
        if tracked {
            changes.push(Change {
                path: root.join(path),
                action: "warning: tracked by git and now contains machine-specific paths",
            });
            continue;
        }
        let pattern = format!("/{path}");
        if !text.lines().any(|l| l.trim() == pattern) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&pattern);
            text.push('\n');
        }
    }
    if text != original {
        changes.push(write_changed(&exclude, Some(&original), &text)?);
    }
    Ok(changes)
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::new(
            "setup",
            "paths written to agent configuration must be UTF-8",
        )
    })
}

fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
