use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::is_within;

pub const MAX_READ_PATH_BYTES: usize = 4096;
pub const MAX_READ_FILE_BYTES: usize = 1_000_000;
pub const MAX_READ_LINES: usize = 2000;
pub const MAX_READ_LINE_NUMBER: usize = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadInput {
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadResult {
    pub path: PathBuf,
    pub size: usize,
    pub total_lines: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub truncated: bool,
    pub text: String,
}

#[derive(Debug)]
pub struct AuthorizedRead {
    path: PathBuf,
    file: File,
    size: usize,
    offset: usize,
    limit: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ReadPolicy {
    roots: Vec<PathBuf>,
    pub max_output_bytes: usize,
}

impl ReadPolicy {
    pub fn new(roots: Vec<PathBuf>, max_output_bytes: usize) -> Result<Self> {
        let mut approved = Vec::new();
        for root in roots {
            let lexical = normalize(&root)?;
            let canonical = std::fs::canonicalize(&root).with_context(|| {
                format!("approved read root is unavailable: {}", root.display())
            })?;
            approved.push(lexical);
            approved.push(canonical);
        }
        approved.sort();
        approved.dedup();
        Ok(Self {
            roots: approved,
            max_output_bytes,
        })
    }

    pub fn authorize(&self, input: &ReadInput, cwd: &Path) -> Result<AuthorizedRead> {
        validate_input(input)?;
        let requested = if Path::new(&input.path).is_absolute() {
            normalize(Path::new(&input.path))?
        } else {
            normalize(&cwd.join(&input.path))?
        };
        if !is_within(&requested, &self.roots) {
            bail!("file is outside approved roots: {}", input.path);
        }
        let canonical = std::fs::canonicalize(&requested)
            .map_err(|_| anyhow::anyhow!("file is unavailable: {}", input.path))?;
        if !is_within(&canonical, &self.roots) {
            bail!(
                "canonical file path is outside approved roots: {}",
                input.path
            );
        }

        let file = open_absolute_no_follow(&canonical, false)
            .map_err(|_| anyhow::anyhow!("file is unavailable: {}", input.path))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("inspect opened file: {}", input.path))?;
        if !metadata.is_file() {
            bail!("path is not a regular file: {}", input.path);
        }
        let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        if size > MAX_READ_FILE_BYTES {
            bail!(
                "file exceeds the {MAX_READ_FILE_BYTES} byte read limit: {}",
                input.path
            );
        }
        Ok(AuthorizedRead {
            path: canonical,
            file,
            size,
            offset: input.offset.unwrap_or(1),
            limit: input.limit,
        })
    }

    pub fn read(&self, input: &ReadInput, cwd: &Path) -> Result<ReadResult> {
        let authorized = self.authorize(input, cwd)?;
        let AuthorizedRead {
            path,
            file,
            size: authorized_size,
            offset,
            limit,
        } = authorized;
        let mut bytes = Vec::with_capacity(authorized_size.saturating_add(1));
        file.take((MAX_READ_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .with_context(|| format!("read file: {}", input.path))?;
        if bytes.len() > MAX_READ_FILE_BYTES {
            bail!(
                "file exceeds the {MAX_READ_FILE_BYTES} byte read limit: {}",
                input.path
            );
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("file is not valid UTF-8 text: {}", input.path))?;
        let lines = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect::<Vec<_>>();
        if offset > lines.len() {
            bail!(
                "offset {offset} is beyond end of file ({} lines)",
                lines.len()
            );
        }
        let start = offset - 1;
        let end = lines
            .len()
            .min(start.saturating_add(limit.unwrap_or(lines.len())));
        let formatted = format_lines(&lines, start, end, self.max_output_bytes);
        Ok(ReadResult {
            path,
            size: text.len(),
            total_lines: lines.len(),
            start_line: offset,
            end_line: formatted.end_line,
            truncated: formatted.truncated || end < lines.len(),
            text: formatted.text,
        })
    }
}

fn validate_input(input: &ReadInput) -> Result<()> {
    if input.path.is_empty() || input.path.len() > MAX_READ_PATH_BYTES {
        bail!("file path length is outside read policy bounds");
    }
    if input.path.as_bytes().contains(&0) {
        bail!("NUL bytes are forbidden");
    }
    if input
        .offset
        .is_some_and(|value| value == 0 || value > MAX_READ_LINE_NUMBER)
    {
        bail!("offset is outside read policy bounds");
    }
    if input
        .limit
        .is_some_and(|value| value == 0 || value > MAX_READ_LINES)
    {
        bail!("limit is outside read policy bounds");
    }
    Ok(())
}

struct FormattedLines {
    text: String,
    end_line: usize,
    truncated: bool,
}

fn format_lines(lines: &[&str], start: usize, end: usize, max_bytes: usize) -> FormattedLines {
    let mut output = Vec::new();
    for index in start..end {
        let line_number = index + 1;
        let rendered = format!("{line_number}\t{}", lines[index]);
        let has_more = index + 1 < lines.len();
        let continuation = if has_more {
            format!(
                "\n[Output truncated at {max_bytes} bytes. Showing lines {}-{line_number} of {}. Use offset={} to continue.]",
                start + 1,
                lines.len(),
                line_number + 1
            )
        } else {
            String::new()
        };
        let candidate = format!(
            "{}{}{}",
            output.join("\n"),
            if output.is_empty() { "" } else { "\n" },
            rendered
        );
        if candidate.len() + continuation.len() <= max_bytes {
            output.push(rendered);
            continue;
        }
        let prior_end = line_number - 1;
        if !output.is_empty() {
            let notice = format!(
                "[Output truncated at {max_bytes} bytes. Showing lines {}-{prior_end} of {}. Use offset={line_number} to continue.]",
                start + 1,
                lines.len()
            );
            return FormattedLines {
                text: format!("{}\n{notice}", output.join("\n")),
                end_line: prior_end,
                truncated: true,
            };
        }
        let notice = format!(
            "\n[Output truncated at {max_bytes} bytes on line {line_number} of {}. Use offset={line_number} to reread the line.]",
            lines.len()
        );
        let prefix = format!("{line_number}\t");
        let available = max_bytes.saturating_sub(prefix.len() + notice.len());
        return FormattedLines {
            text: format!("{prefix}{}{notice}", truncate_utf8(lines[index], available)),
            end_line: line_number,
            truncated: true,
        };
    }

    let end_line = (start + 1).max(end);
    if end < lines.len() {
        output.push(format!(
            "[Showing lines {}-{end_line} of {}. Use offset={} to continue.]",
            start + 1,
            lines.len(),
            end_line + 1
        ));
    }
    FormattedLines {
        text: output.join("\n"),
        end_line,
        truncated: false,
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn normalize(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

pub(crate) fn open_absolute_no_follow(path: &Path, directory: bool) -> std::io::Result<File> {
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path must be absolute",
        ));
    }
    let root = CString::new("/").expect("root path has no NUL");
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut current = unsafe { OwnedFd::from_raw_fd(root_fd) };
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Ok(File::from(current));
    }
    for (index, component) in components.iter().enumerate() {
        let name = CString::new(component.as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL")
        })?;
        let last = index + 1 == components.len();
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        if !last || directory {
            flags |= libc::O_DIRECTORY;
        }
        let fd = unsafe {
            libc::openat(
                std::os::fd::AsRawFd::as_raw_fd(&current),
                name.as_ptr(),
                flags,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        current = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    Ok(File::from(current))
}
