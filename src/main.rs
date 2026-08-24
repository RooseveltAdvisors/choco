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
    replies: Vec<Reply>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        None => run_tui(path),
    }
}

enum Command {
    Post { channel: String, title: String },
    Reply { task_id: String, body: String },
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
    let file_name = path.file_name().ok_or("board path has no file name")?;
    let temp = path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        process::id()
    ));
    {
        let mut file = File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(temp, path)?;
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
        board.tasks.push(Task {
            id: new_id(),
            channel: channel.into(),
            title: title.into(),
            replies: Vec::new(),
        });
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
        if (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Char('q')
        {
            return Ok(true);
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Channels,
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Tasks,
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

    fn compose_with_editor(
        &mut self,
        terminal: &mut DefaultTerminal,
        mode: EditorMode,
    ) -> Result<(), Box<dyn Error>> {
        let existing = match mode {
            EditorMode::EditTask => self
                .selected_task()
                .map(|task| task.title.clone())
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
        let input = content.trim().to_string();
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
                    board.tasks.push(Task {
                        id,
                        channel: channel.clone(),
                        title: input.clone(),
                        replies: Vec::new(),
                    });
                }
                EditorMode::EditTask => {
                    let task = board
                        .tasks
                        .iter_mut()
                        .find(|task| Some(task.id.as_str()) == task_id.as_deref())
                        .ok_or("selected task disappeared after external change")?;
                    task.title = input.clone();
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
                        body: input.clone(),
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
                " j/k move   h/l panes   Enter edit   n new task   r reply   q quit",
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
                .wrap(Wrap { trim: true }),
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
    fn default_board_has_a_channel() {
        let board = Board::default();
        assert_eq!(board.version, 1);
        assert_eq!(board.channels[0].id, "general");
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
        };

        app.submit_input(EditorMode::NewTask, "first task").unwrap();
        assert_eq!(app.selected_task().unwrap().title, "first task");

        app.submit_input(EditorMode::NewTask, "second task")
            .unwrap();
        let mut external = load_board(&path).unwrap();
        external.tasks.reverse();
        write_board(&path, &external).unwrap();

        app.submit_input(EditorMode::EditTask, "renamed task")
            .unwrap();
        assert_eq!(app.selected_task().unwrap().title, "renamed task");
        assert_eq!(app.task_idx, 0);

        let _ = fs::remove_file(path);
    }
}
