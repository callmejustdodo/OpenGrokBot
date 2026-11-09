use std::fs;
use std::path::{Component, Path, PathBuf};

use formation_proto::agent::{Agent, AgentFile, AgentFiles, Avatar, HarnessKind};
use formation_proto::common::{AgentId, ApiError, ErrorCode};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::ports::Result;

pub(super) fn io_error(error: std::io::Error) -> ApiError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::AlreadyExists => ErrorCode::Conflict,
        std::io::ErrorKind::PermissionDenied => ErrorCode::Denied,
        _ => ErrorCode::Internal,
    };
    ApiError::new(code, error.to_string())
}
fn denied() -> ApiError {
    ApiError::new(
        ErrorCode::Denied,
        "path is outside editable agent files or contains a symlink",
    )
}

pub(super) fn validate_path(path: &Path, deleting: bool) -> Result<PathBuf> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(denied());
    }
    if path == Path::new("AGENTS.md") {
        return if deleting {
            Err(denied())
        } else {
            Ok(path.to_path_buf())
        };
    }
    if let Ok(tail) = path.strip_prefix(".claude/skills")
        && tail.components().next().is_some()
    {
        return Ok(path.to_path_buf());
    }
    Err(denied())
}

/// Reject symlinks in the root and every existing component, including the leaf.
pub(super) fn safe_path(root: &Path, relative: &Path) -> Result<()> {
    let root_metadata = fs::symlink_metadata(root).map_err(io_error)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(denied());
    }
    // Stored paths are canonical at creation. This also catches an ancestor
    // replaced by a symlink after the agent was created.
    if root.canonicalize().map_err(io_error)? != root {
        return Err(denied());
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(denied());
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(denied()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

pub(super) fn editable_metadata(root: &Path) -> Result<PathBuf> {
    safe_path(root, Path::new("formation.toml"))?;
    Ok(root.join("formation.toml"))
}

#[derive(Serialize)]
struct Metadata<'a> {
    name: &'a str,
    role: &'a str,
    harness: HarnessKind,
    repositories: &'a [PathBuf],
    avatar: &'a Avatar,
}
pub(super) fn metadata(agent: &Agent) -> Result<String> {
    toml::to_string_pretty(&Metadata {
        name: &agent.name,
        role: &agent.role,
        harness: agent.harness,
        repositories: &agent.repositories,
        avatar: &agent.avatar,
    })
    .map_err(|e| ApiError::new(ErrorCode::InvalidParams, e.to_string()))
}

pub(super) enum CreatedFolder {
    New {
        path: PathBuf,
        existed: bool,
    },
    /// An existing folder: only what Formation added, in the order it was added.
    Adopted {
        added: Vec<PathBuf>,
    },
}
impl CreatedFolder {
    pub fn rollback(self) -> Result<()> {
        match self {
            Self::New { path, existed } => {
                fs::remove_dir_all(&path).map_err(io_error)?;
                if existed {
                    fs::create_dir(&path).map_err(io_error)?;
                }
            }
            Self::Adopted { added } => {
                for path in added.iter().rev() {
                    if path.is_dir() {
                        fs::remove_dir(path).map_err(io_error)?;
                    } else {
                        fs::remove_file(path).map_err(io_error)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// The canonical path of `path` when it is an existing folder, `None` when nothing
/// is there. A symlink or a file there is refused.
pub(super) fn existing_folder(path: &Path) -> Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(Some(path.canonicalize().map_err(io_error)?)),
        Ok(_) => Err(ApiError::new(
            ErrorCode::Conflict,
            "agent folder is not a folder",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(e)),
    }
}

/// Makes an existing folder (canonical `agent.folder_path`) an agent folder by adding
/// only what is missing: `.claude/skills/`, `AGENTS.md`, `CLAUDE.md` and
/// `formation.toml`. Files already there, `AGENTS.md` and `CLAUDE.md` included, are
/// never changed. A folder that already has a `formation.toml` belongs to another
/// agent and is refused.
pub(super) fn adopt(agent: &mut Agent, instructions: &str) -> Result<CreatedFolder> {
    let root = agent.folder_path.clone();
    safe_path(&root, Path::new(""))?;
    match fs::symlink_metadata(root.join("formation.toml")) {
        Ok(_) => {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "the folder already holds another agent's formation.toml",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    let mut added = Vec::new();
    let result = (|| {
        for dir in [".claude", ".claude/skills"] {
            safe_path(&root, Path::new(dir))?;
            let path = root.join(dir);
            match fs::create_dir(&path) {
                Ok(()) => added.push(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
                Err(e) => return Err(io_error(e)),
            }
        }
        let metadata = metadata(agent)?;
        for (name, content) in [
            ("AGENTS.md", instructions),
            ("CLAUDE.md", "@AGENTS.md\n"),
            ("formation.toml", metadata.as_str()),
        ] {
            safe_path(&root, Path::new(name))?;
            let path = root.join(name);
            let mut file = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        && name != "formation.toml" =>
                {
                    continue;
                }
                Err(e) => return Err(io_error(e)),
            };
            added.push(path);
            std::io::Write::write_all(&mut file, content.as_bytes()).map_err(io_error)?;
        }
        Ok(())
    })();
    let folder = CreatedFolder::Adopted { added };
    if let Err(error) = result {
        folder.rollback()?;
        return Err(error);
    }
    // Only noted: the owner's own CLAUDE.md is theirs to change.
    if let Ok(claude) = fs::read(root.join("CLAUDE.md"))
        && !claude.windows(10).any(|w| w == b"@AGENTS.md")
    {
        eprintln!(
            "agent {}: {} has a CLAUDE.md without @AGENTS.md; it was left unchanged",
            agent.slug,
            root.display()
        );
    }
    match revision(&root) {
        Ok(revision) => {
            agent.files_revision = revision;
            Ok(folder)
        }
        Err(error) => {
            folder.rollback()?;
            Err(error)
        }
    }
}

pub(super) fn create(agent: &mut Agent, instructions: &str) -> Result<CreatedFolder> {
    let parent = agent.folder_path.parent().ok_or_else(denied)?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let parent = parent.canonicalize().map_err(io_error)?;
    let name = agent.folder_path.file_name().ok_or_else(denied)?;
    let target = parent.join(name);
    let existed = match fs::symlink_metadata(&target) {
        Ok(meta) => {
            if meta.file_type().is_symlink()
                || !meta.is_dir()
                || fs::read_dir(&target).map_err(io_error)?.next().is_some()
            {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "agent folder is not empty",
                ));
            }
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(io_error(e)),
    };
    let stage = parent.join(format!(".formation-create-{}", agent.id));
    fs::create_dir(&stage).map_err(io_error)?;
    let result = (|| {
        fs::create_dir_all(stage.join(".claude/skills")).map_err(io_error)?;
        fs::write(stage.join("AGENTS.md"), instructions).map_err(io_error)?;
        fs::write(stage.join("CLAUDE.md"), "@AGENTS.md\n").map_err(io_error)?;
        fs::write(stage.join("formation.toml"), metadata(agent)?).map_err(io_error)?;
        // rename replaces only an empty directory, never a populated folder.
        fs::rename(&stage, &target).map_err(|e| {
            if target.exists() {
                ApiError::new(ErrorCode::Conflict, "agent folder is occupied")
            } else {
                io_error(e)
            }
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        fs::remove_dir_all(&stage).map_err(io_error)?;
        return Err(error);
    }
    agent.folder_path = target.clone();
    let folder = CreatedFolder::New {
        path: target,
        existed,
    };
    match revision(&agent.folder_path) {
        Ok(revision) => {
            agent.files_revision = revision;
            Ok(folder)
        }
        Err(error) => {
            folder.rollback()?;
            Err(error)
        }
    }
}

/// Same-directory replacement keeps readers from observing a truncated file.
pub(super) fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_file_name(format!(".formation-write-{}", AgentId::generate()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(io_error)?;
        std::io::Write::write_all(&mut file, bytes).map_err(io_error)?;
        fs::rename(&temporary, path).map_err(io_error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

struct Entry {
    path: PathBuf,
    content: Option<Vec<u8>>,
}
fn entries(root: &Path) -> Result<Vec<Entry>> {
    safe_path(root, Path::new(""))?;
    let mut entries = Vec::new();
    for relative in ["AGENTS.md", "CLAUDE.md", "formation.toml", ".claude/skills"] {
        let path = Path::new(relative);
        safe_path(root, path)?;
        let metadata = match fs::symlink_metadata(root.join(path)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(error)),
        };
        if metadata.is_dir() && relative == ".claude/skills" {
            entries.push(Entry {
                path: path.to_path_buf(),
                content: None,
            });
            visit(root, path, &mut entries)?;
        } else if metadata.is_file() {
            entries.push(Entry {
                path: path.to_path_buf(),
                content: Some(fs::read(root.join(path)).map_err(io_error)?),
            });
        } else {
            return Err(denied());
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}
fn visit(root: &Path, relative: &Path, entries: &mut Vec<Entry>) -> Result<()> {
    for entry in fs::read_dir(root.join(relative)).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type().map_err(io_error)?;
        if kind.is_symlink() {
            return Err(denied());
        }
        if kind.is_dir() {
            entries.push(Entry {
                path: path.clone(),
                content: None,
            });
            visit(root, &path, entries)?;
        } else if kind.is_file() {
            entries.push(Entry {
                path: path.clone(),
                content: Some(fs::read(root.join(&path)).map_err(io_error)?),
            });
        } else {
            return Err(denied());
        }
    }
    Ok(())
}
fn digest(entries: &[Entry]) -> String {
    let mut hash = Sha256::new();
    for entry in entries {
        let path = entry.path.as_os_str().as_encoded_bytes();
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path);
        match &entry.content {
            Some(bytes) => {
                hash.update([1]);
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
            None => hash.update([0]),
        }
    }
    format!("{:x}", hash.finalize())
}
pub(super) fn revision(root: &Path) -> Result<String> {
    Ok(digest(&entries(root)?))
}

pub(super) fn read(agent: &Agent) -> Result<AgentFiles> {
    let entries = entries(&agent.folder_path)?;
    let revision = digest(&entries);
    let mut files = Vec::new();
    for entry in entries {
        if let Some(content) = entry.content
            && validate_path(&entry.path, false).is_ok()
        {
            files.push(AgentFile {
                path: entry.path,
                content: String::from_utf8(content)
                    .map_err(|e| ApiError::new(ErrorCode::InvalidParams, e.to_string()))?,
            });
        }
    }
    Ok(AgentFiles {
        agent_id: agent.id,
        files,
        revision,
    })
}
