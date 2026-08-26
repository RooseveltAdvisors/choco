use std::{
    collections::hash_map::DefaultHasher,
    env,
    error::Error,
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    DefaultTerminal, Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};

const DEFAULT_FILE: &str = "choco.json";
const TASK_REPLIES_MARKER: &str = "--- Replies (preserved on save) ---";
const MARKDOWN_TARGETED_UPDATE: &str = "markdown boards require a targeted update";
const EXTERNAL_CHANGE: &str = "board changed externally; reload before saving";
const SEARCH_LABELS: &str = "asdfghjklqwertyuiopzxcvbnm";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Board {
    #[serde(default = "default_version")]
    version: u8,
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(default)]
    task_order: Option<TaskOrder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskOrder {
    NewestFirst,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Channel {
    id: String,
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    id: String,
    channel: String,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    replies: Vec<Reply>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    id: String,
    author: String,
    body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileMarker {
    modified: SystemTime,
    len: u64,
    hash: u64,
}

#[derive(Debug, Clone)]
struct MarkdownCard {
    start: usize,
    heading_end: usize,
    end: usize,
    title: String,
}

#[derive(Debug, Clone)]
struct MarkdownDocument {
    source: String,
    cards: Vec<MarkdownCard>,
}

enum MarkdownChange {
    Post {
        title: String,
    },
    Edit {
        task_index: usize,
        title: String,
        body: String,
    },
    Reply {
        task_index: usize,
        author: String,
        body: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Channels,
    Tasks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorMode {
    NewTask,
    EditTask,
    Reply,
}

struct App {
    path: PathBuf,
    board: Board,
    channel_idx: usize,
    task_idx: usize,
    focus: Focus,
    file_marker: Option<FileMarker>,
    status: String,
    detail_scroll: u16,
    search_query: Option<String>,
    search_input: Option<String>,
    search_selecting: bool,
    pending_g: bool,
}

impl Default for Board {
    fn default() -> Self {
        Self {
            version: 1,
            channels: vec![Channel {
                id: "general".into(),
                name: "general".into(),
            }],
            tasks: Vec::new(),
            task_order: Some(TaskOrder::NewestFirst),
        }
    }
}

fn default_version() -> u8 {
    1
}

fn main() -> Result<(), Box<dyn Error>> {
    let (path, command) = parse_args()?;
    match command {
        Some(Command::Post { channel, title }) => post_task(&path, &channel, &title),
        Some(Command::Reply { task_id, body }) => reply_to_task(&path, &task_id, &body),
        Some(Command::Render { output }) => render_markdown(&path, &output),
        None => run_tui(path),
    }
}

enum Command {
    Post { channel: String, title: String },
    Reply { task_id: String, body: String },
    Render { output: PathBuf },
}

fn parse_args() -> Result<(PathBuf, Option<Command>), Box<dyn Error>> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let mut path = PathBuf::from(DEFAULT_FILE);
    if args.first().map(String::as_str) == Some("--file") {
        args.drain(..1);
        path = PathBuf::from(args.first().ok_or("--file needs a path")?);
        args.drain(..1);
    }

    let command = match args.first().map(String::as_str) {
        None => None,
        Some("post") => {
            args.remove(0);
            let mut channel = "general".to_string();
            if args.first().map(String::as_str) == Some("--channel")
                || args.first().map(String::as_str) == Some("-c")
            {
                args.remove(0);
                channel = args.first().ok_or("--channel needs a name")?.clone();
                args.remove(0);
            }
            let title = args.join(" ");
            if title.trim().is_empty() {
                return Err("post needs a title".into());
            }
            Some(Command::Post { channel, title })
        }
        Some("reply") => {
            args.remove(0);
            let task_id = args.first().ok_or("reply needs a task id")?.clone();
            args.remove(0);
            let body = args.join(" ");
            if body.trim().is_empty() {
                return Err("reply needs text".into());
            }
            Some(Command::Reply { task_id, body })
        }
        Some("render") => {
            args.remove(0);
            if matches!(
                args.first().map(String::as_str),
                Some("--markdown") | Some("--output") | Some("-o")
            ) {
                args.remove(0);
            }
            let output = PathBuf::from(args.first().ok_or("render needs an output path")?);
            args.remove(0);
            if !args.is_empty() {
                return Err("render accepts one output path".into());
            }
            Some(Command::Render { output })
        }
        Some(command) => return Err(format!("unknown command: {command}").into()),
    };
    Ok((path, command))
}

fn load_board(path: &Path) -> Result<Board, Box<dyn Error>> {
    if !path.exists() {
        return Ok(Board::default());
    }
    if is_markdown_store(path) {
        return load_markdown_board(path);
    }
    let mut board: Board = serde_json::from_reader(File::open(path)?)?;
    if board.version != 1 {
        return Err(format!("unsupported board version: {}", board.version).into());
    }
    if board.task_order.is_none() {
        board.tasks.reverse();
        board.task_order = Some(TaskOrder::NewestFirst);
    }
    Ok(board)
}

fn is_markdown_store(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn load_markdown_board(path: &Path) -> Result<Board, Box<dyn Error>> {
    let document = parse_markdown_document(fs::read_to_string(path)?);
    let tasks = document
        .cards
        .iter()
        .enumerate()
        .map(|(index, card)| {
            markdown_task(
                index,
                card.title.clone(),
                &document.source[card.heading_end..card.end],
            )
        })
        .collect();

    Ok(Board {
        channels: vec![Channel {
            id: "general".into(),
            name: "general".into(),
        }],
        tasks,
        task_order: Some(TaskOrder::NewestFirst),
        ..Board::default()
    })
}

fn markdown_heading(line: &str) -> Option<&str> {
    let line = line.trim_end_matches(['\r', '\n']);
    let title = line.strip_prefix("# ")?.trim_end();
    let hash_start = title.trim_end_matches('#').len();
    let hash_count = title.len() - hash_start;
    if hash_start == 0 {
        return None;
    }
    let title = if hash_count > 0
        && title[..hash_start]
            .chars()
            .last()
            .is_some_and(|character| character.is_whitespace())
    {
        title[..hash_start].trim_end()
    } else {
        title
    };
    (!title.is_empty()).then_some(title)
}

fn markdown_line_ending(source: &str) -> &'static str {
    if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn finish_markdown_card(
    cards: &mut Vec<MarkdownCard>,
    current: &mut Option<(usize, usize, String)>,
    end: usize,
) {
    if let Some((start, heading_end, title)) = current.take() {
        cards.push(MarkdownCard {
            start,
            heading_end,
            end,
            title,
        });
    }
}

fn parse_markdown_document(source: String) -> MarkdownDocument {
    let mut cards = Vec::new();
    let mut current = None;
    let mut fence = None;
    let mut offset = 0;

    for line in source.split_inclusive('\n') {
        if fence.is_none()
            && let Some(title) = markdown_heading(line)
        {
            finish_markdown_card(&mut cards, &mut current, offset);
            current = Some((offset, offset + line.len(), title.to_owned()));
        }
        if let Some(spec) = fence {
            if fence_closes(line, spec) {
                fence = None;
            }
        } else if let Some(spec) = fence_starts(line) {
            fence = Some(spec);
        }
        offset += line.len();
    }
    finish_markdown_card(&mut cards, &mut current, source.len());

    MarkdownDocument { source, cards }
}

fn fence_starts(line: &str) -> Option<(char, usize)> {
    let (character, length, _) = fence_parts(line)?;
    Some((character, length))
}

fn fence_closes(line: &str, (character, length): (char, usize)) -> bool {
    let Some((closing_character, closing_length, suffix)) = fence_parts(line) else {
        return false;
    };
    closing_character == character && closing_length >= length && suffix.trim().is_empty()
}

fn fence_parts(line: &str) -> Option<(char, usize, &str)> {
    let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
    if indentation > 3 {
        return None;
    }
    let rest = &line[indentation..];
    let character = rest.chars().next()?;
    if character != '`' && character != '~' {
        return None;
    }
    let length = rest.chars().take_while(|item| *item == character).count();
    (length >= 3).then(|| {
        let suffix = &rest[character.len_utf8() * length..];
        if character == '`' && suffix.contains('`') {
            return None;
        }
        Some((character, length, suffix))
    })?
}

fn markdown_task(index: usize, title: String, body: &str) -> Task {
    let lines = body.lines().collect::<Vec<_>>();
    Task {
        id: format!("markdown-{}", index + 1),
        channel: "general".into(),
        title,
        body: lines
            .iter()
            .position(|line| !line.trim().is_empty())
            .zip(lines.iter().rposition(|line| !line.trim().is_empty()))
            .map(|(start, end)| lines[start..=end].join("\n"))
            .unwrap_or_default(),
        replies: Vec::new(),
    }
}

fn marker(path: &Path) -> io::Result<Option<FileMarker>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let bytes = fs::read(path)?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok(Some(FileMarker {
        modified: metadata.modified()?,
        len: metadata.len(),
        hash: hasher.finish(),
    }))
}

struct BoardLock {
    path: PathBuf,
}

impl BoardLock {
    fn acquire(board_path: &Path) -> Result<Self, Box<dyn Error>> {
        let file_name = board_path
            .file_name()
            .ok_or("board path has no file name")?;
        let path = board_path.with_file_name(format!(".{}.lock", file_name.to_string_lossy()));
        for _ in 0..50 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err("timed out waiting for board lock".into())
    }
}

impl Drop for BoardLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
fn write_board(path: &Path, board: &Board) -> Result<(), Box<dyn Error>> {
    if is_markdown_store(path) {
        return Err(MARKDOWN_TARGETED_UPDATE.into());
    }
    let bytes = serde_json::to_vec_pretty(board)?;
    write_atomically(path, &bytes, "board")
}

fn write_board_if_unchanged(
    path: &Path,
    board: &Board,
    expected: Option<FileMarker>,
) -> Result<(), Box<dyn Error>> {
    if is_markdown_store(path) {
        return Err(MARKDOWN_TARGETED_UPDATE.into());
    }
    let bytes = serde_json::to_vec_pretty(board)?;
    write_atomically_if_unchanged(path, &bytes, "board", Some(expected))
}

fn write_atomically(path: &Path, contents: &[u8], description: &str) -> Result<(), Box<dyn Error>> {
    write_atomically_if_unchanged(path, contents, description, None)
}

fn write_atomically_if_unchanged(
    path: &Path,
    contents: &[u8],
    description: &str,
    expected: Option<Option<FileMarker>>,
) -> Result<(), Box<dyn Error>> {
    if let Some(expected) = expected
        && marker(path)? != expected
    {
        return Err(EXTERNAL_CHANGE.into());
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("{description} path has no file name"))?;
    let (temp, mut file) = {
        let mut created = None;
        for attempt in 0..100 {
            let temp = path.with_file_name(format!(
                ".{}.{}.{}.tmp",
                file_name.to_string_lossy(),
                process::id(),
                attempt
            ));
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(file) => {
                    created = Some((temp, file));
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        created.ok_or("could not create a unique temporary file")?
    };
    if let Err(error) = file.write_all(contents).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    if let Some(expected) = expected
        && marker(path)? != expected
    {
        let _ = fs::remove_file(&temp);
        return Err(EXTERNAL_CHANGE.into());
    }
    if expected == Some(None) {
        match fs::hard_link(&temp, path) {
            Ok(()) => {
                let _ = fs::remove_file(temp);
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temp);
                return Err(EXTERNAL_CHANGE.into());
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(error.into());
            }
        }
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(temp);
        return Err(error.into());
    }
    Ok(())
}

fn update_board<F>(path: &Path, update: F) -> Result<Board, Box<dyn Error>>
where
    F: FnOnce(&mut Board) -> Result<(), Box<dyn Error>>,
{
    if is_markdown_store(path) {
        return Err(MARKDOWN_TARGETED_UPDATE.into());
    }
    let _lock = BoardLock::acquire(path)?;
    let expected = marker(path)?;
    let mut board = load_board(path)?;
    update(&mut board)?;
    write_board_if_unchanged(path, &board, expected)?;
    Ok(board)
}

fn post_task(path: &Path, channel: &str, title: &str) -> Result<(), Box<dyn Error>> {
    if is_markdown_store(path) {
        if channel != "general" {
            return Err("Markdown stores support only the general channel".into());
        }
        let expected = marker(path)?;
        update_markdown(
            path,
            Some(expected),
            MarkdownChange::Post {
                title: title.trim().into(),
            },
        )?;
        println!("posted to #{channel}");
        return Ok(());
    }
    update_board(path, |board| {
        if !board.channels.iter().any(|item| item.id == channel) {
            board.channels.push(Channel {
                id: channel.into(),
                name: channel.into(),
            });
        }
        board.tasks.insert(
            0,
            Task {
                id: new_id(),
                channel: channel.into(),
                title: title.into(),
                body: String::new(),
                replies: Vec::new(),
            },
        );
        Ok(())
    })?;
    println!("posted to #{channel}");
    Ok(())
}

fn reply_to_task(path: &Path, task_id: &str, body: &str) -> Result<(), Box<dyn Error>> {
    if is_markdown_store(path) {
        let expected = marker(path)?;
        let board = load_board(path)?;
        let task_index = board
            .tasks
            .iter()
            .position(|task| task.id == task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        update_markdown(
            path,
            Some(expected),
            MarkdownChange::Reply {
                task_index,
                author: author(),
                body: body.trim().into(),
            },
        )?;
        println!("replied to {task_id}");
        return Ok(());
    }
    update_board(path, |board| {
        let task = board
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        task.replies.push(Reply {
            id: new_id(),
            author: author(),
            body: body.into(),
        });
        Ok(())
    })?;
    println!("replied to {task_id}");
    Ok(())
}

fn markdown_task_index(task_id: &str) -> Result<usize, Box<dyn Error>> {
    let index = task_id
        .strip_prefix("markdown-")
        .ok_or_else(|| format!("invalid Markdown task id: {task_id}"))?
        .parse::<usize>()
        .map_err(|_| format!("invalid Markdown task id: {task_id}"))?;
    index
        .checked_sub(1)
        .ok_or_else(|| format!("invalid Markdown task id: {task_id}").into())
}

fn selected_markdown_task_index(task_id: Option<&str>) -> Result<usize, Box<dyn Error>> {
    markdown_task_index(task_id.ok_or("selected task disappeared after external change")?)
}

fn markdown_card(
    document: &MarkdownDocument,
    task_index: usize,
) -> Result<&MarkdownCard, Box<dyn Error>> {
    document
        .cards
        .get(task_index)
        .ok_or_else(|| format!("task not found: markdown-{}", task_index + 1).into())
}

fn markdown_change(
    mode: EditorMode,
    task_id: Option<&str>,
    text: String,
    body: String,
) -> Result<MarkdownChange, Box<dyn Error>> {
    match mode {
        EditorMode::NewTask => Ok(MarkdownChange::Post { title: text }),
        EditorMode::EditTask => Ok(MarkdownChange::Edit {
            task_index: selected_markdown_task_index(task_id)?,
            title: text,
            body,
        }),
        EditorMode::Reply => Ok(MarkdownChange::Reply {
            task_index: selected_markdown_task_index(task_id)?,
            author: author(),
            body: text,
        }),
    }
}

fn replace_markdown_card(
    document: &MarkdownDocument,
    card: &MarkdownCard,
    replacement: &str,
) -> String {
    let mut result = document.source.clone();
    result.replace_range(card.start..card.end, replacement);
    result
}

fn update_markdown(
    path: &Path,
    expected: Option<Option<FileMarker>>,
    change: MarkdownChange,
) -> Result<Board, Box<dyn Error>> {
    let _lock = BoardLock::acquire(path)?;
    let before = marker(path)?;
    if let Some(expected) = expected
        && before != expected
    {
        return Err(EXTERNAL_CHANGE.into());
    }
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    let document = parse_markdown_document(source);
    let markdown = apply_markdown_change(&document, change)?;
    write_atomically_if_unchanged(path, markdown.as_bytes(), "markdown", Some(before))?;
    load_markdown_board(path)
}

fn apply_markdown_change(
    document: &MarkdownDocument,
    change: MarkdownChange,
) -> Result<String, Box<dyn Error>> {
    match change {
        MarkdownChange::Post { title } => {
            let line_ending = markdown_line_ending(&document.source);
            if let Some(first_card) = document.cards.first() {
                let card = format!("# {title}{line_ending}{line_ending}");
                let mut result = document.source.clone();
                result.insert_str(first_card.start, &card);
                return Ok(result);
            }
            let card = format!("# {title}{line_ending}");
            let blank_line = format!("{line_ending}{line_ending}");
            let separator = if document.source.is_empty() || document.source.ends_with(&blank_line)
            {
                ""
            } else if document.source.ends_with(line_ending) {
                line_ending
            } else {
                &blank_line
            };
            Ok(format!("{}{separator}{card}", document.source))
        }
        MarkdownChange::Edit {
            task_index,
            title,
            body,
        } => {
            let card = markdown_card(document, task_index)?;
            let raw_body = &document.source[card.heading_end..card.end];
            let leading_len = raw_body.len() - raw_body.trim_start_matches(['\r', '\n']).len();
            let trailing_len = raw_body.len() - raw_body.trim_end_matches(['\r', '\n']).len();
            let content_end = raw_body.len().saturating_sub(trailing_len);
            let (leading_end, content_start) = if leading_len > content_end {
                (raw_body.len(), raw_body.len())
            } else {
                (leading_len, content_end)
            };
            let line_ending = markdown_line_ending(&document.source[card.start..card.heading_end]);
            let mut replacement = format!("# {title}{line_ending}");
            replacement.push_str(&raw_body[..leading_end]);
            if !body.is_empty() {
                replacement.push_str(&body);
            }
            replacement.push_str(&raw_body[content_start..]);
            Ok(replace_markdown_card(document, card, &replacement))
        }
        MarkdownChange::Reply {
            task_index,
            author,
            body,
        } => {
            let card = markdown_card(document, task_index)?;
            let raw_body = &document.source[card.heading_end..card.end];
            let trailing_len = raw_body.len() - raw_body.trim_end_matches(['\r', '\n']).len();
            let content_end = raw_body.len().saturating_sub(trailing_len);
            let content = &raw_body[..content_end];
            let suffix = &raw_body[content_end..];
            let line_ending = markdown_line_ending(&document.source[card.start..card.heading_end]);
            let separator = if content.is_empty() || content.ends_with(['\r', '\n']) {
                ""
            } else {
                line_ending
            };
            let mut replacement = document.source[card.start..card.end].to_owned();
            let heading_len = card.heading_end - card.start;
            replacement.truncate(heading_len);
            replacement.push_str(content);
            replacement.push_str(separator);
            for (index, line) in body.lines().enumerate() {
                replacement.push_str("> ");
                if index == 0 {
                    replacement.push_str(&author);
                    replacement.push_str(": ");
                }
                replacement.push_str(line);
                replacement.push_str(line_ending);
            }
            replacement.push_str(suffix);
            Ok(replace_markdown_card(document, card, &replacement))
        }
    }
}

fn render_markdown(path: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    if paths_refer_to_same_file(path, output)? {
        return Err("render output must differ from the board path".into());
    }
    let _lock = BoardLock::acquire(path)?;
    if is_markdown_store(path) {
        let source = fs::read(path)?;
        let task_count = parse_markdown_document(String::from_utf8(source.clone())?)
            .cards
            .len();
        write_atomically(output, &source, "markdown")?;
        println!(
            "rendered {} tasks to {}",
            task_count,
            output.display()
        );
        return Ok(());
    }
    let board = load_board(path)?;
    let markdown = board_to_markdown(&board);
    write_markdown(output, &markdown)?;
    println!(
        "rendered {} tasks to {}",
        board.tasks.len(),
        output.display()
    );
    Ok(())
}

fn board_to_markdown(board: &Board) -> String {
    let mut markdown = String::new();
    for (index, task) in board.tasks.iter().enumerate() {
        if index > 0 {
            markdown.push('\n');
        }
        markdown.push_str("# ");
        markdown.push_str(&task.title);
        markdown.push_str("\n\n");
        markdown.push_str("<!-- choco: channel=");
        markdown.push_str(&task.channel);
        markdown.push_str(" id=");
        markdown.push_str(&task.id);
        markdown.push_str(" -->\n");
        if !task.body.is_empty() {
            markdown.push('\n');
            markdown.push_str(&task.body);
            markdown.push('\n');
        }
        for reply in &task.replies {
            markdown.push('\n');
            markdown.push_str("> ");
            if !(reply.author.eq_ignore_ascii_case("firstmate")
                && reply.body.starts_with("Firstmate:"))
            {
                markdown.push_str(&reply.author);
                markdown.push_str(": ");
            }
            for (index, line) in reply.body.lines().enumerate() {
                if index > 0 {
                    markdown.push_str("\n> ");
                }
                markdown.push_str(line);
            }
            markdown.push('\n');
        }
    }
    markdown
}

fn write_markdown(path: &Path, markdown: &str) -> Result<(), Box<dyn Error>> {
    write_atomically(path, markdown.as_bytes(), "markdown")
}

fn paths_refer_to_same_file(first: &Path, second: &Path) -> io::Result<bool> {
    let first = comparable_path(first)?;
    let second = comparable_path(second)?;
    if first == second {
        return Ok(true);
    }

    #[cfg(unix)]
    {
        let first_metadata = match fs::metadata(first) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let second_metadata = match fs::metadata(second) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(first_metadata.dev() == second_metadata.dev()
            && first_metadata.ino() == second_metadata.ino())
    }

    #[cfg(not(unix))]
    Ok(false)
}

fn comparable_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return Ok(canonical);
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let file_name = absolute
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    Ok(fs::canonicalize(parent)?.join(file_name))
}

fn new_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!(
        "{millis}-{}-{}",
        process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn author() -> String {
    env::var("CHOCO_AUTHOR")
        .or_else(|_| env::var("USER"))
        .unwrap_or_else(|_| "agent".into())
}

fn run_tui(path: PathBuf) -> Result<(), Box<dyn Error>> {
    let board = load_board(&path)?;
    let mut app = App {
        file_marker: marker(&path)?,
        path,
        board,
        channel_idx: 0,
        task_idx: 0,
        focus: Focus::Tasks,
        status: "Ready".into(),
        detail_scroll: 0,
        search_query: None,
        search_input: None,
        search_selecting: false,
        pending_g: false,
    };
    let mut terminal = setup_terminal()?;
    let result = app.run(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<DefaultTerminal, Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut DefaultTerminal) -> Result<(), Box<dyn Error>> {
    terminal.show_cursor()?;
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

fn editor_temp_dir() -> io::Result<PathBuf> {
    let base = env::temp_dir();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..100 {
        let path = base.join(format!("choco-compose-{}-{stamp}-{attempt}", process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique editor buffer",
    ))
}

fn launch_editor(path: &Path) -> io::Result<process::ExitStatus> {
    let command = env::var("EDITOR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "nvim".into());
    let mut parts = split_command(&command)?;
    let executable = parts.remove(0);
    process::Command::new(executable)
        .args(parts)
        .arg(path)
        .status()
}

fn split_command(command: &str) -> io::Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut token_started = false;

    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            token_started = true;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
            token_started = true;
        } else if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
            token_started = true;
        } else if character.is_whitespace() {
            if token_started {
                args.push(std::mem::take(&mut current));
                token_started = false;
            }
        } else {
            current.push(character);
            token_started = true;
        }
    }

    if escaped || quote.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "EDITOR has an unfinished quote or escape",
        ));
    }
    if token_started {
        args.push(current);
    }
    if args.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "EDITOR did not contain a command",
        ));
    }
    Ok(args)
}

fn read_editor_buffer(path: &Path) -> io::Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(path).map(Some)
}

fn task_editor_buffer(task: &Task) -> String {
    let mut buffer = format!("{}\n\n{}", task.title, task.body);
    buffer.push_str(if task.body.is_empty() { "\n" } else { "\n\n" });
    buffer.push_str(TASK_REPLIES_MARKER);
    for reply in &task.replies {
        buffer.push_str(&format!("\n\n{}:\n{}", reply.author, reply.body));
    }
    buffer.push('\n');
    buffer
}

fn preserved_editor_suffix(replies: &[Reply]) -> String {
    let mut suffix = format!("\n\n{TASK_REPLIES_MARKER}");
    for reply in replies {
        suffix.push_str(&format!("\n\n{}:\n{}", reply.author, reply.body));
    }
    suffix.push('\n');
    suffix
}

fn parse_task_editor(
    content: &str,
    preserved_replies: &[Reply],
) -> Result<(String, String), Box<dyn Error>> {
    let (title, rest) = content
        .split_once('\n')
        .ok_or("task editor needs a title")?;
    let normalized_rest = rest.trim_end_matches('\n');
    let suffix = preserved_editor_suffix(preserved_replies);
    let body = normalized_rest
        .strip_suffix(suffix.trim_end_matches('\n'))
        .or_else(|| normalized_rest.strip_suffix(&format!("\n\n{TASK_REPLIES_MARKER}")))
        .unwrap_or(normalized_rest);
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("task title cannot be empty".into());
    }
    Ok((title, body.trim().to_string()))
}

fn task_matches(task: &Task, query: &str) -> bool {
    let query = query.to_lowercase();
    task.title.to_lowercase().contains(&query)
        || task.body.to_lowercase().contains(&query)
        || task.replies.iter().any(|reply| {
            reply.author.to_lowercase().contains(&query)
                || reply.body.to_lowercase().contains(&query)
        })
}

impl App {
    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<(), Box<dyn Error>> {
        loop {
            self.check_external_change()?;
            terminal.draw(|frame| self.draw(frame))?;
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            if let Event::Key(key) = event::read()?
                && self.handle_key(key, terminal)?
            {
                break;
            }
        }
        Ok(())
    }

    fn check_external_change(&mut self) -> Result<(), Box<dyn Error>> {
        let current = marker(&self.path)?;
        if current == self.file_marker {
            return Ok(());
        }
        let channel_id = self.selected_channel().map(|channel| channel.id.clone());
        let task_id = self.selected_task().map(|task| task.id.clone());
        match load_board(&self.path) {
            Ok(board) => {
                self.file_marker = current;
                self.board = board;
                self.restore_selection(channel_id.as_deref(), task_id.as_deref());
                self.status = "reloaded external changes".into();
            }
            Err(error) => {
                self.status =
                    format!("external board unavailable - keeping current data ({error})");
            }
        }
        Ok(())
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut DefaultTerminal,
    ) -> Result<bool, Box<dyn Error>> {
        if self.search_input.is_some() {
            self.handle_search_key(key);
            return Ok(false);
        }
        if (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Char('q')
        {
            return Ok(true);
        }
        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                self.jump_to_boundary(false);
                return Ok(false);
            }
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Channels,
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Tasks,
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('G') => self.jump_to_boundary(true),
            KeyCode::Char('/') => {
                self.search_input = Some(String::new());
                self.search_selecting = false;
                self.update_search_status();
            }
            KeyCode::Char('n') if self.search_query.is_some() => self.find_match(1),
            KeyCode::Char('N') if self.search_query.is_some() => self.find_match(-1),
            KeyCode::Esc => {
                self.pending_g = false;
                self.search_query = None;
                self.status = "Ready".into();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10);
                self.status = format!("details scrolled to {}", self.detail_scroll);
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.detail_scroll = self.detail_scroll.saturating_add(10);
                self.status = format!("details scrolled to {}", self.detail_scroll);
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Channels => Focus::Tasks,
                    Focus::Tasks => Focus::Channels,
                }
            }
            KeyCode::Enter => {
                if self.focus == Focus::Tasks && self.selected_task().is_some() {
                    self.compose_with_editor(terminal, EditorMode::EditTask)?;
                } else {
                    self.focus = Focus::Tasks;
                }
            }
            KeyCode::Char('n') => self.compose_with_editor(terminal, EditorMode::NewTask)?,
            KeyCode::Char('r') if self.selected_task().is_some() => {
                self.compose_with_editor(terminal, EditorMode::Reply)?
            }
            KeyCode::Char('R') => self.reload_now()?,
            _ => {}
        }
        Ok(false)
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        let Some(input) = self.search_input.clone() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.search_input = None;
                self.search_selecting = false;
                self.search_query = None;
                self.status = "search cancelled".into();
            }
            KeyCode::Enter => {
                let query = input.trim().to_string();
                if query.is_empty() {
                    self.search_input = None;
                    self.search_selecting = false;
                    self.search_query = None;
                    self.status = "search cleared".into();
                } else {
                    self.search_input = Some(query.clone());
                    self.search_query = Some(query);
                    self.search_selecting = true;
                    self.update_search_status();
                }
            }
            KeyCode::Backspace => {
                self.search_selecting = false;
                if let Some(input) = self.search_input.as_mut() {
                    input.pop();
                }
                self.update_search_status();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.search_selecting {
                    if let Some(task_idx) = self.search_label_target(character) {
                        self.select_search_candidate(task_idx, character);
                    }
                } else if let Some(input) = self.search_input.as_mut() {
                    input.push(character);
                    self.update_search_status();
                }
            }
            _ => {}
        }
    }

    fn update_search_status(&mut self) {
        let query = self.search_input.as_deref().unwrap_or_default();
        let shown = self.search_candidates().len();
        let total = self.search_match_count();
        self.status = format!(
            "/{query}  {}{} candidates  {}",
            shown,
            if shown < total {
                format!(" of {total}")
            } else {
                String::new()
            },
            if self.search_selecting {
                "press a letter to jump"
            } else {
                "type to narrow, Enter labels"
            }
        );
    }

    fn search_candidates(&self) -> Vec<(usize, &Task)> {
        let Some(query) = self.search_input.as_deref() else {
            return Vec::new();
        };
        self.visible_tasks()
            .into_iter()
            .enumerate()
            .filter(|(_, task)| task_matches(task, query))
            .take(SEARCH_LABELS.chars().count())
            .collect()
    }

    fn search_match_count(&self) -> usize {
        let Some(query) = self.search_input.as_deref() else {
            return 0;
        };
        self.visible_tasks()
            .into_iter()
            .filter(|task| task_matches(task, query))
            .count()
    }

    fn search_label_target(&self, label: char) -> Option<usize> {
        for (candidate_idx, (task_idx, _)) in self.search_candidates().into_iter().enumerate() {
            if SEARCH_LABELS.chars().nth(candidate_idx) == Some(label) {
                return Some(task_idx);
            }
        }
        None
    }

    fn select_search_candidate(&mut self, task_idx: usize, label: char) {
        let query = self.search_input.take().unwrap_or_default();
        self.search_selecting = false;
        self.search_query = if query.is_empty() {
            None
        } else {
            Some(query.clone())
        };
        self.task_idx = task_idx;
        self.focus = Focus::Tasks;
        self.detail_scroll = 0;
        self.status = format!("/{query}  [{label}] selected");
    }

    fn find_match(&mut self, delta: isize) {
        let Some(query) = self.search_query.as_deref() else {
            return;
        };
        let query = query.to_lowercase();
        let tasks = self.visible_tasks();
        if tasks.is_empty() {
            self.status = format!("no matches for {}", query);
            return;
        }
        for step in 1..=tasks.len() {
            let index = (self.task_idx as isize + delta * step as isize)
                .rem_euclid(tasks.len() as isize) as usize;
            if task_matches(tasks[index], &query) {
                self.task_idx = index;
                self.focus = Focus::Tasks;
                self.detail_scroll = 0;
                self.status = format!("/{query}");
                return;
            }
        }
        self.status = format!("no matches for {query}");
    }

    fn jump_to_boundary(&mut self, last: bool) {
        match self.focus {
            Focus::Channels => {
                self.channel_idx = if last {
                    self.board.channels.len().saturating_sub(1)
                } else {
                    0
                };
            }
            Focus::Tasks => {
                self.task_idx = if last {
                    self.visible_tasks().len().saturating_sub(1)
                } else {
                    0
                };
            }
        }
        self.detail_scroll = 0;
    }

    fn compose_with_editor(
        &mut self,
        terminal: &mut DefaultTerminal,
        mode: EditorMode,
    ) -> Result<(), Box<dyn Error>> {
        let existing = match mode {
            EditorMode::EditTask => self
                .selected_task()
                .map(task_editor_buffer)
                .unwrap_or_default(),
            EditorMode::NewTask | EditorMode::Reply => String::new(),
        };
        let temp_dir = editor_temp_dir()?;
        let temp_path = temp_dir.join("compose.txt");

        if !existing.is_empty() {
            fs::write(&temp_path, &existing)?;
        }
        let before = marker(&temp_path)?;

        restore_terminal(terminal)?;
        let editor_result = launch_editor(&temp_path);
        *terminal = setup_terminal()?;

        let editor_status = match editor_result {
            Ok(status) => status,
            Err(error) => {
                let _ = fs::remove_dir_all(&temp_dir);
                self.status = format!("could not launch editor - {error}");
                return Ok(());
            }
        };
        if !editor_status.success() {
            let _ = fs::remove_dir_all(&temp_dir);
            self.status = "draft discarded".into();
            return Ok(());
        }
        if marker(&temp_path)? == before {
            let _ = fs::remove_dir_all(&temp_dir);
            self.status = "draft discarded - write to submit".into();
            return Ok(());
        }
        let content = match read_editor_buffer(&temp_path)? {
            Some(content) if !content.trim().is_empty() => content,
            None | Some(_) => {
                let _ = fs::remove_dir_all(&temp_dir);
                self.status = "draft discarded".into();
                return Ok(());
            }
        };
        if let Err(error) = self.submit_input(mode, &content) {
            self.status = format!("could not save draft - {error}");
            let _ = fs::remove_dir_all(&temp_dir);
            return Ok(());
        };
        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    fn submit_markdown_input(
        &mut self,
        mode: EditorMode,
        content: &str,
    ) -> Result<(), Box<dyn Error>> {
        let task_id = self.selected_task().map(|task| task.id.clone());
        let (text, body) = match mode {
            EditorMode::EditTask => {
                let replies = self
                    .selected_task()
                    .map(|task| task.replies.clone())
                    .unwrap_or_default();
                parse_task_editor(content, &replies)?
            }
            EditorMode::NewTask | EditorMode::Reply => (content.trim().to_string(), String::new()),
        };
        if text.is_empty() {
            return Err("editor content cannot be empty".into());
        }
        let change = markdown_change(mode, task_id.as_deref(), text, body)?;
        let board = update_markdown(&self.path, Some(self.file_marker), change)?;
        let selected_task_id = match mode {
            EditorMode::NewTask => board.tasks.first().map(|task| task.id.clone()),
            EditorMode::EditTask | EditorMode::Reply => task_id,
        };
        self.board = board;
        self.file_marker = marker(&self.path)?;
        self.focus = Focus::Tasks;
        self.restore_selection(None, selected_task_id.as_deref());
        self.status = match mode {
            EditorMode::NewTask => "Task saved - selected below".into(),
            EditorMode::EditTask => "Task updated".into(),
            EditorMode::Reply => "Reply saved".into(),
        };
        Ok(())
    }

    fn submit_input(&mut self, mode: EditorMode, content: &str) -> Result<(), Box<dyn Error>> {
        if is_markdown_store(&self.path) {
            return self.submit_markdown_input(mode, content);
        }
        let preserved_replies = if mode == EditorMode::EditTask {
            self.selected_task()
                .map(|task| task.replies.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let (text, body) = match mode {
            EditorMode::EditTask => parse_task_editor(content, &preserved_replies)?,
            EditorMode::NewTask | EditorMode::Reply => (content.trim().to_string(), String::new()),
        };
        if text.is_empty() {
            return Err("editor content cannot be empty".into());
        }
        let channel = self
            .selected_channel()
            .map(|channel| channel.id.clone())
            .unwrap_or_else(|| "general".into());
        let task_id = self.selected_task().map(|task| task.id.clone());
        let mut created_task_id = None;
        let board = update_board(&self.path, |board| {
            match mode {
                EditorMode::NewTask => {
                    if !board.channels.iter().any(|item| item.id == channel) {
                        board.channels.push(Channel {
                            id: channel.clone(),
                            name: channel.clone(),
                        });
                    }
                    let id = new_id();
                    created_task_id = Some(id.clone());
                    board.tasks.insert(
                        0,
                        Task {
                            id,
                            channel: channel.clone(),
                            title: text.clone(),
                            body: String::new(),
                            replies: Vec::new(),
                        },
                    );
                }
                EditorMode::EditTask => {
                    let task = board
                        .tasks
                        .iter_mut()
                        .find(|task| Some(task.id.as_str()) == task_id.as_deref())
                        .ok_or("selected task disappeared after external change")?;
                    task.title = text.clone();
                    task.body = body.clone();
                }
                EditorMode::Reply => {
                    let task = board
                        .tasks
                        .iter_mut()
                        .find(|task| Some(task.id.as_str()) == task_id.as_deref())
                        .ok_or("selected task disappeared after external change")?;
                    task.replies.push(Reply {
                        id: new_id(),
                        author: author(),
                        body: text.clone(),
                    });
                }
            }
            Ok(())
        })?;
        self.board = board;
        self.file_marker = marker(&self.path)?;
        let selected_task_id = created_task_id.or(task_id);
        if selected_task_id.is_some() {
            self.focus = Focus::Tasks;
        }
        self.restore_selection(Some(&channel), selected_task_id.as_deref());
        self.status = match mode {
            EditorMode::NewTask => "Task saved - selected below".into(),
            EditorMode::EditTask => "Task updated".into(),
            EditorMode::Reply => "Reply saved".into(),
        };
        Ok(())
    }

    fn reload_now(&mut self) -> Result<(), Box<dyn Error>> {
        let channel_id = self.selected_channel().map(|channel| channel.id.clone());
        let task_id = self.selected_task().map(|task| task.id.clone());
        self.board = load_board(&self.path)?;
        self.file_marker = marker(&self.path)?;
        self.restore_selection(channel_id.as_deref(), task_id.as_deref());
        self.status = "reloaded".into();
        Ok(())
    }

    fn move_selection(&mut self, delta: isize) {
        self.detail_scroll = 0;
        match self.focus {
            Focus::Channels => {
                self.channel_idx = move_index(self.channel_idx, self.board.channels.len(), delta);
                self.task_idx = 0;
            }
            Focus::Tasks => {
                let count = self.visible_tasks().len();
                self.task_idx = move_index(self.task_idx, count, delta);
            }
        }
    }

    fn visible_tasks(&self) -> Vec<&Task> {
        let channel = self.selected_channel().map(|channel| channel.id.as_str());
        self.board
            .tasks
            .iter()
            .filter(|task| channel.is_none() || Some(task.channel.as_str()) == channel)
            .collect()
    }

    fn selected_channel(&self) -> Option<&Channel> {
        self.board.channels.get(self.channel_idx)
    }

    fn selected_task(&self) -> Option<&Task> {
        self.visible_tasks().get(self.task_idx).copied()
    }

    fn clamp_selection(&mut self) {
        self.channel_idx = self
            .channel_idx
            .min(self.board.channels.len().saturating_sub(1));
        self.task_idx = self
            .task_idx
            .min(self.visible_tasks().len().saturating_sub(1));
    }

    fn restore_selection(&mut self, channel_id: Option<&str>, task_id: Option<&str>) {
        let task_channel = task_id.and_then(|id| {
            self.board
                .tasks
                .iter()
                .find(|task| task.id == id)
                .map(|task| task.channel.as_str())
        });
        if let Some(id) = task_channel.or(channel_id)
            && let Some(index) = self
                .board
                .channels
                .iter()
                .position(|channel| channel.id == id)
        {
            self.channel_idx = index;
        }
        if let Some(id) = task_id
            && let Some(index) = self.visible_tasks().iter().position(|task| task.id == id)
        {
            self.task_idx = index;
            return;
        }
        self.clamp_selection();
    }

    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(frame.area());
        let title = Paragraph::new(Text::from(vec![
            Line::from(vec![
                Span::styled(
                    " choco ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("task board", Style::default().add_modifier(Modifier::BOLD)),
            ]),
            Line::from(Span::styled(
                if let Some(query) = &self.search_input {
                    if self.search_selecting {
                        format!(" /{query}  press a letter to jump   Esc cancel")
                    } else {
                        format!(" /{query}  type to narrow   Enter labels   Esc cancel")
                    }
                } else {
                    " hjkl move   gg/G top/bottom   / search   n/N next   Ctrl-u/d scroll   Enter edit   n new task   r reply   q quit".into()
                },
                Style::default().fg(Color::Cyan),
            )),
        ]))
        .block(Block::default().borders(Borders::BOTTOM));
        frame.render_widget(title, chunks[0]);
        self.draw_board(frame, chunks[1]);
        let status =
            Paragraph::new(format!(" {} ", self.status)).style(Style::default().fg(Color::Green));
        frame.render_widget(status, chunks[2]);
    }

    fn draw_board(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(35),
                Constraint::Percentage(45),
            ])
            .split(area);
        self.draw_channels(frame, columns[0]);
        self.draw_tasks(frame, columns[1]);
        self.draw_details(frame, columns[2]);
    }

    fn draw_details(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let lines = if let Some(task) = self.selected_task() {
            let mut lines = vec![
                Line::from(Span::styled(
                    task.title.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!("id: {}", task.id)),
                Line::from(format!("replies: {}", task.replies.len())),
            ];
            if !task.body.is_empty() {
                lines.push(Line::from(""));
                lines.extend(task.body.lines().map(Line::from));
            }
            for reply in &task.replies {
                lines.extend([
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("{}:", reply.author),
                        Style::default().fg(Color::Cyan),
                    )),
                    Line::from(reply.body.clone()),
                ]);
            }
            lines
        } else {
            vec![
                Line::from(Span::styled(
                    "No task selected",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("Press n to create a task."),
            ]
        };
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(
                    Block::default()
                        .title(" Task details ")
                        .borders(Borders::ALL)
                        .border_style(if self.focus == Focus::Tasks {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default()
                        }),
                )
                .wrap(Wrap { trim: true })
                .scroll((self.detail_scroll, 0)),
            area,
        );
    }

    fn draw_channels(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let items = self
            .board
            .channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                let item = ListItem::new(format!("# {}", channel.name));
                if index == self.channel_idx && self.focus == Focus::Channels {
                    item.style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    item
                }
            })
            .collect::<Vec<_>>();
        let block = Block::default()
            .title(" Channels  [h/l] ")
            .borders(Borders::ALL)
            .border_style(if self.focus == Focus::Channels {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            });
        frame.render_widget(List::new(items).block(block), area);
    }

    fn draw_tasks(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let tasks = if self.search_input.is_some() {
            self.search_candidates()
                .into_iter()
                .enumerate()
                .map(|(candidate_idx, (task_idx, task))| {
                    (SEARCH_LABELS.chars().nth(candidate_idx), task_idx, task)
                })
                .collect::<Vec<_>>()
        } else {
            self.visible_tasks()
                .into_iter()
                .enumerate()
                .map(|(task_idx, task)| (None, task_idx, task))
                .collect::<Vec<_>>()
        };
        let items = tasks
            .into_iter()
            .map(|(label, index, task)| {
                let marker = if index == self.task_idx && self.focus == Focus::Tasks {
                    "›"
                } else {
                    " "
                };
                let item = if let Some(label) = label {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{label} "),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(format!(
                            "{marker} {}  ({} replies)",
                            task.title,
                            task.replies.len()
                        )),
                    ]))
                } else {
                    ListItem::new(format!(
                        "{marker} {}  ({} replies)",
                        task.title,
                        task.replies.len()
                    ))
                };
                if index == self.task_idx && self.focus == Focus::Tasks {
                    item.style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    item
                }
            })
            .collect::<Vec<_>>();
        let block = Block::default()
            .title(format!(
                " #{} Tasks  [Enter edit] ",
                self.selected_channel()
                    .map(|item| item.name.as_str())
                    .unwrap_or("?"),
            ))
            .borders(Borders::ALL)
            .border_style(if self.focus == Focus::Tasks {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            });
        frame.render_widget(List::new(items).block(block), area);
    }
}

fn move_index(index: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    ((index as isize + delta).rem_euclid(len as isize)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_wraps_and_empty_lists_stay_safe() {
        assert_eq!(move_index(0, 3, -1), 2);
        assert_eq!(move_index(2, 3, 1), 0);
        assert_eq!(move_index(0, 0, 1), 0);
    }

    #[test]
    fn editor_command_supports_quoted_arguments() {
        assert_eq!(
            split_command("nvim -c 'set number'").unwrap(),
            ["nvim", "-c", "set number"]
        );
        assert!(split_command("nvim 'unfinished").is_err());
    }

    #[test]
    fn task_editor_buffer_includes_body_and_preserves_replies() {
        let task = Task {
            id: "task-1".into(),
            channel: "general".into(),
            title: "A task".into(),
            body: format!("Task context\n{TASK_REPLIES_MARKER}"),
            replies: vec![Reply {
                id: "reply-1".into(),
                author: "captain".into(),
                body: format!("Thread context\n{TASK_REPLIES_MARKER}"),
            }],
        };
        let buffer = task_editor_buffer(&task);
        assert!(buffer.contains("A task"));
        assert!(buffer.contains("Task context"));
        assert!(buffer.contains("captain:\nThread context"));
        assert_eq!(
            parse_task_editor(&buffer, &task.replies).unwrap(),
            (task.title.clone(), task.body.clone())
        );

        let edited = format!("Renamed\n\nUpdated context\n\n{TASK_REPLIES_MARKER}");
        assert_eq!(
            parse_task_editor(&edited, &task.replies).unwrap(),
            ("Renamed".into(), "Updated context".into())
        );
        let edited_by_nvim = format!("Renamed\n\nUpdated context\n\n{TASK_REPLIES_MARKER}\n\n");
        assert_eq!(
            parse_task_editor(&edited_by_nvim, &task.replies).unwrap(),
            ("Renamed".into(), "Updated context".into())
        );

        let empty_body_task = Task {
            body: String::new(),
            ..task
        };
        let empty_body_buffer = task_editor_buffer(&empty_body_task);
        assert_eq!(
            parse_task_editor(&empty_body_buffer, &empty_body_task.replies).unwrap(),
            (empty_body_task.title, String::new())
        );
    }

    #[test]
    fn search_matches_titles_bodies_and_replies() {
        let task = Task {
            id: "task-1".into(),
            channel: "general".into(),
            title: "A task".into(),
            body: "Important context".into(),
            replies: vec![Reply {
                id: "reply-1".into(),
                author: "captain".into(),
                body: "Thread context".into(),
            }],
        };
        assert!(task_matches(&task, "important"));
        assert!(task_matches(&task, "thread"));
        assert!(task_matches(&task, "CAPTAIN"));
        assert!(!task_matches(&task, "missing"));
    }

    #[test]
    fn slash_search_narrows_candidates_and_letter_selects_one() {
        let mut app = App {
            path: PathBuf::new(),
            board: Board {
                tasks: vec![
                    Task {
                        id: "alpha".into(),
                        channel: "general".into(),
                        title: "Alpha task".into(),
                        body: String::new(),
                        replies: Vec::new(),
                    },
                    Task {
                        id: "beta".into(),
                        channel: "general".into(),
                        title: "Beta task".into(),
                        body: String::new(),
                        replies: Vec::new(),
                    },
                    Task {
                        id: "alphabet".into(),
                        channel: "general".into(),
                        title: "Alphabet task".into(),
                        body: String::new(),
                        replies: Vec::new(),
                    },
                ],
                ..Board::default()
            },
            channel_idx: 0,
            task_idx: 0,
            focus: Focus::Tasks,
            file_marker: None,
            status: String::new(),
            detail_scroll: 0,
            search_query: None,
            search_input: Some(String::new()),
            search_selecting: false,
            pending_g: false,
        };

        app.handle_search_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(app.search_candidates().len(), 3);
        app.handle_search_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert_eq!(app.search_input.as_deref(), Some("al"));
        assert_eq!(app.search_candidates().len(), 2);
        assert_eq!(app.search_label_target('s'), Some(2));

        app.handle_search_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.search_selecting);
        app.handle_search_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(app.search_input.is_none());
        assert_eq!(app.search_query.as_deref(), Some("al"));
        assert_eq!(app.selected_task().unwrap().id, "alphabet");
        assert!(app.status.contains("[s] selected"));

        app.search_input = Some("al".into());
        app.search_selecting = false;
        app.handle_search_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.search_selecting);
        app.handle_search_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(app.selected_task().unwrap().id, "alpha");

        app.search_input = Some("al".into());
        app.search_query = Some("al".into());
        app.search_selecting = true;
        app.handle_search_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.search_query.is_none());
        assert!(app.search_input.is_none());
        assert!(!app.search_selecting);
    }

    fn search_test_app(tasks: Vec<Task>) -> App {
        App {
            path: PathBuf::new(),
            board: Board {
                tasks,
                ..Board::default()
            },
            channel_idx: 0,
            task_idx: 0,
            focus: Focus::Tasks,
            file_marker: None,
            status: String::new(),
            detail_scroll: 0,
            search_query: None,
            search_input: None,
            search_selecting: false,
            pending_g: false,
        }
    }

    fn press_search(app: &mut App, code: KeyCode) {
        app.handle_search_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn render_screen(app: &App) -> String {
        render_screen_sized(app, 120, 20)
    }

    fn render_screen_sized(app: &App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        format!("{}", terminal.backend())
    }

    #[test]
    fn slash_search_caps_labels_at_26_and_indicates_total() {
        let tasks = (0..30)
            .map(|i| Task {
                id: format!("t{i}"),
                channel: "general".into(),
                title: format!("Task {i:02}"),
                body: String::new(),
                replies: Vec::new(),
            })
            .collect();
        let mut app = search_test_app(tasks);
        app.search_input = Some(String::new());
        press_search(&mut app, KeyCode::Char('t'));

        assert_eq!(app.search_candidates().len(), 26);
        assert_eq!(app.search_match_count(), 30);
        assert!(app.status.contains("26 of 30"));
        assert!(app.status.contains("type to narrow, Enter labels"));
        assert_eq!(app.search_label_target('a'), Some(0));
        assert_eq!(app.search_label_target('m'), Some(25));
        assert!(app.search_label_target('1').is_none());

        let screen = render_screen_sized(&app, 120, 40);
        assert!(screen.contains("a › Task 00"), "{screen}");
        assert!(screen.contains("m   Task 25"), "{screen}");
        assert!(!screen.contains("Task 26"), "{screen}");
        assert!(!screen.contains("Task 29"), "{screen}");
        assert!(screen.contains("26 of 30"), "{screen}");

        press_search(&mut app, KeyCode::Enter);
        assert!(app.search_selecting);
        assert!(app.status.contains("press a letter to jump"));
        let labeled = render_screen(&app);
        assert!(labeled.contains("press a letter to jump"), "{labeled}");

        press_search(&mut app, KeyCode::Char('s'));
        assert_eq!(app.selected_task().unwrap().id, "t1");
        assert!(app.search_input.is_none());
        assert_eq!(app.search_query.as_deref(), Some("t"));
        assert!(app.status.contains("[s] selected"));

        app.find_match(1);
        assert_eq!(app.selected_task().unwrap().id, "t2");
        app.find_match(-1);
        assert_eq!(app.selected_task().unwrap().id, "t1");
    }

    #[test]
    fn slash_search_renders_live_labels_before_enter() {
        let mut app = search_test_app(vec![
            Task {
                id: "alpha".into(),
                channel: "general".into(),
                title: "Alpha task".into(),
                body: String::new(),
                replies: Vec::new(),
            },
            Task {
                id: "beta".into(),
                channel: "general".into(),
                title: "Beta task".into(),
                body: String::new(),
                replies: Vec::new(),
            },
            Task {
                id: "alphabet".into(),
                channel: "general".into(),
                title: "Alphabet task".into(),
                body: String::new(),
                replies: Vec::new(),
            },
        ]);
        app.search_input = Some(String::new());
        press_search(&mut app, KeyCode::Char('a'));
        press_search(&mut app, KeyCode::Char('l'));

        let screen = render_screen(&app);
        assert!(screen.contains("/al"), "{screen}");
        assert!(screen.contains("type to narrow"), "{screen}");
        assert!(screen.contains("Enter labels"), "{screen}");
        assert!(screen.contains("a "), "{screen}");
        assert!(screen.contains("s "), "{screen}");
        assert!(screen.contains("Alpha task"), "{screen}");
        assert!(screen.contains("Alphabet task"), "{screen}");
        assert!(!screen.contains("Beta task"), "{screen}");
        assert!(app.status.contains("2 candidates"), "{}", app.status);
    }

    #[test]
    fn default_board_has_a_channel() {
        let board = Board::default();
        assert_eq!(board.version, 1);
        assert_eq!(board.channels[0].id, "general");
    }

    #[test]
    fn markdown_render_keeps_card_order_and_existing_firstmate_stamps() {
        let board = Board {
            tasks: vec![
                Task {
                    id: "new".into(),
                    channel: "general".into(),
                    title: "Newest".into(),
                    body: "Do this first.".into(),
                    replies: vec![
                        Reply {
                            id: "stamp".into(),
                            author: "firstmate".into(),
                            body: "Firstmate: **08-24-2026 21:16:34.** stamped".into(),
                        },
                        Reply {
                            id: "reply".into(),
                            author: "jon".into(),
                            body: "A reply".into(),
                        },
                    ],
                },
                Task {
                    id: "old".into(),
                    channel: "awaiting".into(),
                    title: "Older".into(),
                    body: String::new(),
                    replies: Vec::new(),
                },
            ],
            ..Board::default()
        };

        let markdown = board_to_markdown(&board);
        assert!(markdown.find("# Newest").unwrap() < markdown.find("# Older").unwrap());
        assert!(markdown.contains("<!-- choco: channel=general id=new -->"));
        assert!(
            markdown.contains("> Firstmate: **08-24-2026 21:16:34.** stamped\n\n> jon: A reply")
        );
    }

    #[test]
    fn markdown_render_preserves_canonical_store_order_and_content() {
        let stem = format!("choco-render-test-{}-{}", process::id(), new_id());
        let board_path = env::temp_dir().join(format!("{stem}.json"));
        let markdown_path = env::temp_dir().join(format!("{stem}.md"));
        let board = Board {
            tasks: vec![
                Task {
                    id: "newest".into(),
                    channel: "general".into(),
                    title: "Current newest".into(),
                    body: "Newest context".into(),
                    replies: Vec::new(),
                },
                Task {
                    id: "middle".into(),
                    channel: "general".into(),
                    title: "Current middle".into(),
                    body: "Middle context".into(),
                    replies: Vec::new(),
                },
                Task {
                    id: "oldest".into(),
                    channel: "general".into(),
                    title: "Current oldest".into(),
                    body: "Oldest context".into(),
                    replies: Vec::new(),
                },
            ],
            ..Board::default()
        };
        write_board(&board_path, &board).unwrap();
        let before = fs::read(&board_path).unwrap();

        render_markdown(&board_path, &markdown_path).unwrap();

        let rendered = fs::read_to_string(&markdown_path).unwrap();
        let titles = rendered
            .lines()
            .filter(|line| line.starts_with("# "))
            .map(|line| line[2..].to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            ["Current newest", "Current middle", "Current oldest"]
        );
        for (title, id, body) in [
            ("Current newest", "newest", "Newest context"),
            ("Current middle", "middle", "Middle context"),
            ("Current oldest", "oldest", "Oldest context"),
        ] {
            let card = format!("# {title}\n\n<!-- choco: channel=general id={id} -->\n\n{body}\n");
            assert!(rendered.contains(&card), "{card}\n{rendered}");
        }
        assert_eq!(fs::read(&board_path).unwrap(), before);

        let _ = fs::remove_file(board_path);
        let _ = fs::remove_file(markdown_path);
    }

    #[test]
    fn markdown_store_reads_and_writes_fixture_without_losing_unknown_content() {
        let stem = format!("choco-markdown-store-test-{}-{}", process::id(), new_id());
        let path = env::temp_dir().join(format!("{stem}.md"));
        let rendered_path = env::temp_dir().join(format!("{stem}-rendered.md"));
        let fixture = "# Current newest\n\n> Firstmate: stamped <!-- zeta-todo-write:1 -->\n\n\
captain context\n    indented code\n\n## nested heading\n\n```text\n# heading in task content\n```\n\n\
# Current oldest #\n\nolder context\n";
        fs::write(&path, fixture).unwrap();

        render_markdown(&path, &rendered_path).unwrap();
        assert_eq!(fs::read(&rendered_path).unwrap(), fixture.as_bytes());

        let board = load_board(&path).unwrap();
        assert_eq!(
            board
                .tasks
                .iter()
                .map(|task| task.title.as_str())
                .collect::<Vec<_>>(),
            ["Current newest", "Current oldest"]
        );
        assert_eq!(
            board.tasks[0].body,
            "> Firstmate: stamped <!-- zeta-todo-write:1 -->\n\n\
             captain context\n    indented code\n\n## nested heading\n\n\
             ```text\n# heading in task content\n```"
        );
        assert_eq!(board.tasks[1].body, "older context");
        assert_eq!(board.tasks[0].channel, "general");
        assert_eq!(board.task_order, Some(TaskOrder::NewestFirst));

        let before_channel_error = fs::read(&path).unwrap();
        assert!(post_task(&path, "awaiting", "wrong channel").is_err());
        assert_eq!(fs::read(&path).unwrap(), before_channel_error);

        let mut app = App {
            path: path.clone(),
            board,
            channel_idx: 0,
            task_idx: 0,
            focus: Focus::Tasks,
            file_marker: marker(&path).unwrap(),
            status: String::new(),
            detail_scroll: 0,
            search_query: None,
            search_input: None,
            search_selecting: false,
            pending_g: false,
        };
        let screen = render_screen_sized(&app, 160, 30);
        assert!(screen.find("Current newest").unwrap() < screen.find("Current oldest").unwrap());
        assert!(screen.contains("captain context"), "{screen}");
        assert!(screen.contains("nested heading"), "{screen}");

        app.submit_input(EditorMode::NewTask, "Appended task")
            .unwrap();
        assert_eq!(app.selected_task().unwrap().title, "Appended task");
        let after_post = fs::read_to_string(&path).unwrap();
        assert!(after_post.starts_with("# Appended task\n\n"));
        assert!(after_post.ends_with(fixture));
        reply_to_task(&path, "markdown-2", "new reply").unwrap();
        let after_write = fs::read_to_string(&path).unwrap();
        assert!(after_write.contains("> Firstmate: stamped <!-- zeta-todo-write:1 -->"));
        assert!(after_write.contains("## nested heading"));
        assert!(after_write.contains("# Current oldest #\n\nolder context\n"));
        assert!(after_write.contains("> jon: new reply"));
        assert!(after_write.contains("# Appended task"));

        let expected = marker(&path).unwrap().unwrap();
        let edited = update_markdown(
            &path,
            Some(Some(expected)),
            MarkdownChange::Edit {
                task_index: 0,
                title: "Renamed newest".into(),
                body: "updated context".into(),
            },
        )
        .unwrap();
        assert_eq!(edited.tasks[0].title, "Renamed newest");
        let after_edit = fs::read_to_string(&path).unwrap();
        assert!(after_edit.contains("# Renamed newest"));
        assert!(after_edit.contains("# Current oldest #\n\nolder context\n"));

        let expected = marker(&path).unwrap().unwrap();
        fs::write(&path, format!("{after_edit}\nexternal edit\n")).unwrap();
        let unchanged = fs::read(&path).unwrap();
        assert!(
            update_markdown(
                &path,
                Some(Some(expected)),
                MarkdownChange::Reply {
                    task_index: 0,
                    author: "jon".into(),
                    body: "must not overwrite".into(),
                },
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), unchanged);
        let _ = fs::remove_file(path);

        let missing_path = env::temp_dir().join(format!("{stem}-missing.md"));
        post_task(&missing_path, "general", "Created from empty file").unwrap();
        assert_eq!(
            fs::read_to_string(&missing_path).unwrap(),
            "# Created from empty file\n"
        );
        let _ = fs::remove_file(missing_path);
        let _ = fs::remove_file(rendered_path);
    }

    #[test]
    fn markdown_render_migrates_legacy_task_order_without_external_imports() {
        let stem = format!("choco-render-test-{}-{}", process::id(), new_id());
        let board_path = env::temp_dir().join(format!("{stem}.json"));
        let markdown_path = env::temp_dir().join(format!("{stem}.md"));
        let board = Board {
            tasks: vec![
                Task {
                    id: "old".into(),
                    channel: "general".into(),
                    title: "Older".into(),
                    body: String::new(),
                    replies: Vec::new(),
                },
                Task {
                    id: "new".into(),
                    channel: "general".into(),
                    title: "Newest".into(),
                    body: String::new(),
                    replies: Vec::new(),
                },
            ],
            task_order: None,
            ..Board::default()
        };
        write_board(&board_path, &board).unwrap();
        fs::write(
            &markdown_path,
            "# Older\n\n<!-- choco: channel=general id=old -->\n\n\
             # Newest\n\n<!-- choco: channel=general id=new -->\n\n\
             > Firstmate: **08-24-2026 21:16:34.** stamped\n",
        )
        .unwrap();

        render_markdown(&board_path, &markdown_path).unwrap();

        let rendered = fs::read_to_string(&markdown_path).unwrap();
        assert!(rendered.find("# Newest").unwrap() < rendered.find("# Older").unwrap());
        assert!(!rendered.contains("> Firstmate: **08-24-2026 21:16:34.** stamped"));

        let _ = fs::remove_file(board_path);
        let _ = fs::remove_file(markdown_path);
    }

    #[test]
    fn markdown_render_rejects_overwriting_the_json_board() {
        let path = env::temp_dir().join(format!(
            "choco-render-test-{}-{}.json",
            process::id(),
            new_id()
        ));
        write_board(&path, &Board::default()).unwrap();
        let before = fs::read(&path).unwrap();

        assert!(render_markdown(&path, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_submission_selects_new_task_and_updates_existing_task() {
        let path = env::temp_dir().join(format!("choco-test-{}-{}.json", process::id(), new_id()));
        let mut app = App {
            path: path.clone(),
            board: Board::default(),
            channel_idx: 0,
            task_idx: 0,
            focus: Focus::Tasks,
            file_marker: None,
            status: String::new(),
            detail_scroll: 0,
            search_query: None,
            search_input: None,
            search_selecting: false,
            pending_g: false,
        };

        app.submit_input(EditorMode::NewTask, "first task").unwrap();
        assert_eq!(app.selected_task().unwrap().title, "first task");

        app.submit_input(EditorMode::NewTask, "second task")
            .unwrap();
        assert_eq!(app.board.tasks[0].title, "second task");
        app.submit_input(EditorMode::Reply, "existing reply")
            .unwrap();
        let replies = app.selected_task().unwrap().replies.clone();
        let mut external = load_board(&path).unwrap();
        external.tasks.reverse();
        write_board(&path, &external).unwrap();

        app.submit_input(
            EditorMode::EditTask,
            &format!("renamed task\n\nupdated body\n\n{TASK_REPLIES_MARKER}"),
        )
        .unwrap();
        assert_eq!(app.selected_task().unwrap().title, "renamed task");
        assert_eq!(app.selected_task().unwrap().body, "updated body");
        assert_eq!(app.selected_task().unwrap().replies, replies);
        assert_eq!(
            load_board(&path)
                .unwrap()
                .tasks
                .iter()
                .find(|task| task.title == "renamed task")
                .unwrap()
                .replies,
            replies
        );
        assert_eq!(app.selected_task().unwrap().title, "renamed task");

        let _ = fs::remove_file(path);
    }
}
