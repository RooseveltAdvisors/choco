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
enum View {
    Board,
    Thread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    NewTask,
    Reply,
}

struct App {
    path: PathBuf,
    board: Board,
    channel_idx: usize,
    task_idx: usize,
    focus: Focus,
    view: View,
    input_mode: Option<InputMode>,
    input: String,
    file_marker: Option<FileMarker>,
    reload_pending: bool,
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
        view: View::Board,
        input_mode: None,
        input: String::new(),
        reload_pending: false,
        status: "j/k move  Enter open  n new  r reply  q quit".into(),
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

impl App {
    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<(), Box<dyn Error>> {
        loop {
            self.check_external_change()?;
            terminal.draw(|frame| self.draw(frame))?;
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            if let Event::Key(key) = event::read()?
                && self.handle_key(key)?
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
        if self.input_mode.is_some() {
            self.file_marker = current;
            self.reload_pending = true;
            self.status = "file changed - draft preserved; submit to merge".into();
            return Ok(());
        }
        match load_board(&self.path) {
            Ok(board) => {
                self.file_marker = current;
                self.board = board;
                self.clamp_selection();
                self.status = "reloaded external changes".into();
            }
            Err(error) => {
                self.status =
                    format!("external board unavailable - keeping current data ({error})");
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool, Box<dyn Error>> {
        if self.input_mode.is_some() {
            return self.handle_input_key(key);
        }
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
                    self.view = View::Thread;
                } else {
                    self.focus = Focus::Tasks;
                }
            }
            KeyCode::Esc => self.view = View::Board,
            KeyCode::Char('n') => {
                self.input_mode = Some(InputMode::NewTask);
                self.input.clear();
                self.status = "new task title:".into();
            }
            KeyCode::Char('r') if self.selected_task().is_some() => {
                self.input_mode = Some(InputMode::Reply);
                self.input.clear();
                self.status = "reply:".into();
            }
            KeyCode::Char('R') => self.reload_now()?,
            _ => {}
        }
        Ok(false)
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Result<bool, Box<dyn Error>> {
        match key.code {
            KeyCode::Esc => {
                let reload_pending = self.reload_pending;
                self.input_mode = None;
                self.input.clear();
                self.reload_pending = false;
                if reload_pending {
                    self.file_marker = None;
                    self.status = "draft discarded; reloading external changes".into();
                } else {
                    self.status = "draft discarded".into();
                }
            }
            KeyCode::Enter if !self.input.trim().is_empty() => self.submit_input()?,
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(character);
            }
            _ => {}
        }
        Ok(false)
    }

    fn submit_input(&mut self) -> Result<(), Box<dyn Error>> {
        let mode = self.input_mode.ok_or("no input active")?;
        let input = self.input.trim().to_string();
        let channel = self
            .selected_channel()
            .map(|channel| channel.id.clone())
            .unwrap_or_else(|| "general".into());
        let task_id = self.selected_task().map(|task| task.id.clone());
        let result = update_board(&self.path, |board| {
            match mode {
                InputMode::NewTask => {
                    if !board.channels.iter().any(|item| item.id == channel) {
                        board.channels.push(Channel {
                            id: channel.clone(),
                            name: channel.clone(),
                        });
                    }
                    board.tasks.push(Task {
                        id: new_id(),
                        channel: channel.clone(),
                        title: input.clone(),
                        replies: Vec::new(),
                    });
                }
                InputMode::Reply => {
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
        });
        let board = match result {
            Ok(board) => board,
            Err(error) => {
                self.status = format!("could not save draft - {error}");
                return Ok(());
            }
        };
        self.board = board;
        self.file_marker = marker(&self.path)?;
        self.input_mode = None;
        self.input.clear();
        self.reload_pending = false;
        self.clamp_selection();
        self.status = "saved".into();
        Ok(())
    }

    fn reload_now(&mut self) -> Result<(), Box<dyn Error>> {
        if self.input_mode.is_some() {
            self.status = "finish or Esc the draft before reloading".into();
            return Ok(());
        }
        self.board = load_board(&self.path)?;
        self.file_marker = marker(&self.path)?;
        self.clamp_selection();
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

    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(frame.area());
        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                " choco ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("task board"),
        ]))
        .block(Block::default().borders(Borders::BOTTOM));
        frame.render_widget(title, chunks[0]);
        match self.view {
            View::Board => self.draw_board(frame, chunks[1]),
            View::Thread => self.draw_thread(frame, chunks[1]),
        }
        let status = if let Some(mode) = self.input_mode {
            let label = match mode {
                InputMode::NewTask => "new task",
                InputMode::Reply => "reply",
            };
            Paragraph::new(format!(" {label}> {}", self.input))
                .style(Style::default().fg(Color::Cyan))
        } else {
            Paragraph::new(format!(" {}", self.status))
        };
        frame.render_widget(status, chunks[2]);
    }

    fn draw_board(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
            .split(area);
        self.draw_channels(frame, columns[0]);
        self.draw_tasks(frame, columns[1]);
    }

    fn draw_thread(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(30),
                Constraint::Percentage(50),
            ])
            .split(area);
        self.draw_channels(frame, columns[0]);
        self.draw_tasks(frame, columns[1]);
        let lines = if let Some(task) = self.selected_task() {
            let mut lines = vec![Line::from(Span::styled(
                &task.title,
                Style::default().add_modifier(Modifier::BOLD),
            ))];
            lines.extend(task.replies.iter().flat_map(|reply| {
                [
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("{}:", reply.author),
                        Style::default().fg(Color::Cyan),
                    )),
                    Line::from(reply.body.clone()),
                ]
            }));
            lines
        } else {
            vec![Line::from("No task selected")]
        };
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().title(" thread ").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            columns[2],
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
        let block = Block::default().title(" channels ").borders(Borders::ALL);
        frame.render_widget(List::new(items).block(block), area);
    }

    fn draw_tasks(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let items = self
            .visible_tasks()
            .iter()
            .enumerate()
            .map(|(index, task)| {
                let item = ListItem::new(format!("{}  {}", task.id, task.title));
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
                " #{} tasks - {} ",
                self.selected_channel()
                    .map(|item| item.name.as_str())
                    .unwrap_or("?"),
                if self.view == View::Board {
                    "Enter opens thread"
                } else {
                    "Esc closes thread"
                }
            ))
            .borders(Borders::ALL);
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
    fn default_board_has_a_channel() {
        let board = Board::default();
        assert_eq!(board.version, 1);
        assert_eq!(board.channels[0].id, "general");
    }
}
