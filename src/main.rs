use std::{
    collections::hash_map::DefaultHasher,
    env,
    error::Error,
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    process, thread,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Board {
    #[serde(default = "default_version")]
    version: u8,
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default)]
    tasks: Vec<Task>,
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
    let board: Board = serde_json::from_reader(File::open(path)?)?;
    if board.version != 1 {
        return Err(format!("unsupported board version: {}", board.version).into());
    }
    Ok(board)
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

fn write_board(path: &Path, board: &Board) -> Result<(), Box<dyn Error>> {
    let bytes = serde_json::to_vec_pretty(board)?;
    write_atomically(path, &bytes, "board")
}

fn write_atomically(path: &Path, contents: &[u8], description: &str) -> Result<(), Box<dyn Error>> {
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
    let _lock = BoardLock::acquire(path)?;
    let mut board = load_board(path)?;
    update(&mut board)?;
    write_board(path, &board)?;
    Ok(board)
}

fn post_task(path: &Path, channel: &str, title: &str) -> Result<(), Box<dyn Error>> {
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

fn render_markdown(path: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    if paths_refer_to_same_file(path, output)? {
        return Err("render output must differ from the JSON board path".into());
    }
    let _lock = BoardLock::acquire(path)?;
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
    format!("{millis}-{}", process::id())
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
                self.status = "/".into();
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
        let Some(input) = self.search_input.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.search_input = None;
                self.status = "search cancelled".into();
            }
            KeyCode::Enter => {
                let query = input.trim().to_string();
                self.search_input = None;
                if query.is_empty() {
                    self.search_query = None;
                    self.status = "search cleared".into();
                } else {
                    self.search_query = Some(query);
                    self.find_match(1);
                }
            }
            KeyCode::Backspace => {
                input.pop();
                self.status = format!("/{}", input);
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                input.push(character);
                self.status = format!("/{}", input);
            }
            _ => {}
        }
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

    fn submit_input(&mut self, mode: EditorMode, content: &str) -> Result<(), Box<dyn Error>> {
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
                    format!(" /{query}  Enter search   Esc cancel")
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
        let items = self
            .visible_tasks()
            .iter()
            .enumerate()
            .map(|(index, task)| {
                let marker = if index == self.task_idx && self.focus == Focus::Tasks {
                    "›"
                } else {
                    " "
                };
                let item = ListItem::new(format!(
                    "{marker} {}  ({} replies)",
                    task.title,
                    task.replies.len()
                ));
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
