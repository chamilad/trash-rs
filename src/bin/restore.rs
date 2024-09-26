use std::env;
use std::error::Error;
use std::fs::{read_dir, read_to_string};
use std::path::PathBuf;

use crossterm::event::{self, Event, KeyCode};
use libtrash::*;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::DisableMouseCapture;
use ratatui::crossterm::event::EnableMouseCapture;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use urlencoding::decode;

const VERBOSE_MODE: bool = false;

const SELECTED_FG_COLOR_DIR: Color = Color::Blue;
const SELECTED_FG_COLOR_FILE: Color = Color::White;
const SELECTED_FG_COLOR_LINK: Color = Color::Magenta;
const SELECTED_BG_COLOR: Color = Color::DarkGray;

const UNSELECTED_FG_COLOR_DIR: Color = Color::Blue;
const UNSELECTED_FG_COLOR_FILE: Color = Color::White;
const UNSELECTED_FG_COLOR_LINK: Color = Color::Magenta;

const TITLE_HEIGHT: u16 = 3;
const FOOTER_HEIGHT: u16 = 3;

enum AppState {
    MainScreen,
    DeletionConfirmation(usize),
    Exiting,
}

struct TrashedFile {
    OriginalFile: PathBuf,
    DeletionDate: String,
    File: PathBuf,
}

struct App {
    state: AppState,
    trashed_files: Vec<TrashedFile>,
    selected: usize,
}

impl App {
    fn new(trashed_files: Vec<TrashedFile>) -> Self {
        Self {
            state: AppState::MainScreen,
            trashed_files,
            selected: 0,
        }
    }

    fn render(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(TITLE_HEIGHT),
                    Constraint::Min(3),
                    Constraint::Length(FOOTER_HEIGHT),
                ]
                .as_ref(),
            )
            .split(f.area());

        // ================== title
        let title_block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default());

        let title = Paragraph::new(Text::styled(
            "Rubbish Bin",
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .block(title_block);

        f.render_widget(title, chunks[0]);

        // ================== mid section
        match self.state {
            AppState::MainScreen => {
                let midsection_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
                    .split(chunks[1]);

                let frame_area = f.area();
                let file_list_width = frame_area.width as f32 * 0.6; // 60% of the screen width

                let mut selected_desc: Text = Text::default();
                let mut preview: Text = Text::default();
                let preview_height: i32 =
                    ((frame_area.height as f32 - TITLE_HEIGHT as f32 - FOOTER_HEIGHT as f32) * 0.6)
                        .floor() as i32; // 60% of the midsection height

                // ================= file list
                // todo: sort by deletion date and type (dir first)
                let list_items: Vec<ListItem> = self
                    .trashed_files
                    .iter()
                    .enumerate()
                    .map(|(i, file)| {
                        let original_file_name = file
                            .OriginalFile
                            .file_name()
                            .expect("file_name")
                            .to_os_string()
                            .into_string()
                            .unwrap();

                        let original_path = file.OriginalFile.to_str().unwrap();

                        // Calculate padding to fill the remaining space for full line width
                        let padding =
                            (file_list_width as usize).saturating_sub(original_file_name.len());
                        let padded_str = format!("{}{}", original_file_name, " ".repeat(padding));

                        let f_type: String = if file.File.is_dir() {
                            "Directory".to_string()
                        } else if file.File.is_symlink() {
                            "Link".to_string()
                        } else {
                            "File".to_string()
                        };

                        let entry = if i == self.selected {
                            // generate description
                            selected_desc = Text::from(vec![
                                Line::from(vec![
                                    Span::styled(
                                        "Name: ",
                                        Style::default().add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        original_file_name,
                                        Style::default().fg(Color::Gray),
                                    ),
                                ]),
                                Line::from(vec![
                                    Span::styled(
                                        "Type: ",
                                        Style::default().add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(f_type, Style::default().fg(Color::Gray)),
                                ]),
                                Line::from(vec![
                                    Span::styled(
                                        "Original path: ",
                                        Style::default().add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(original_path, Style::default().fg(Color::Gray)),
                                ]),
                                Line::from(vec![
                                    Span::styled(
                                        "Deleted on: ",
                                        Style::default().add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        file.DeletionDate.clone(),
                                        Style::default().fg(Color::Gray),
                                    ),
                                ]),
                            ]);

                            // generate file preview
                            preview = if file.File.is_dir() {
                                // show contents up to 10
                                let mut lines = vec![];
                                let entries = read_dir(file.File.clone())
                                    .unwrap()
                                    .map(|res| res.map(|e| e.path()))
                                    .collect::<Result<Vec<_>, io::Error>>()
                                    .unwrap();

                                if entries.len() == 0 {
                                    lines.push(Line::from(vec![Span::styled(
                                        "empty directory",
                                        Style::default().fg(Color::Gray),
                                    )]));
                                } else {
                                    for (i, entry) in entries.into_iter().enumerate() {
                                        if i > preview_height as usize {
                                            break;
                                        }

                                        let line = if entry.is_dir() {
                                            Line::from(vec![Span::styled(
                                                entry
                                                    .file_name()
                                                    .unwrap()
                                                    .to_os_string()
                                                    .into_string()
                                                    .unwrap(),
                                                Style::default().fg(UNSELECTED_FG_COLOR_DIR),
                                            )])
                                        } else if entry.is_symlink() {
                                            Line::from(vec![Span::styled(
                                                entry
                                                    .file_name()
                                                    .unwrap()
                                                    .to_os_string()
                                                    .into_string()
                                                    .unwrap(),
                                                Style::default().fg(UNSELECTED_FG_COLOR_LINK),
                                            )])
                                        } else {
                                            Line::from(vec![Span::styled(
                                                entry
                                                    .file_name()
                                                    .unwrap()
                                                    .to_os_string()
                                                    .into_string()
                                                    .unwrap(),
                                                Style::default().fg(UNSELECTED_FG_COLOR_FILE),
                                            )])
                                        };
                                        lines.push(line);
                                    }
                                }
                                Text::from(lines)
                            } else if file.File.is_symlink() {
                                match fs::read_link(file.File.clone()) {
                                    Ok(target_path) => {
                                        let target_path_str =
                                            target_path.to_string_lossy().to_string();
                                        Text::from(vec![Line::from(vec![
                                            Span::styled(
                                                "Original target: ",
                                                Style::default().add_modifier(Modifier::BOLD),
                                            ),
                                            Span::styled(
                                                target_path_str,
                                                Style::default().fg(Color::Gray),
                                            ),
                                        ])])
                                    }
                                    Err(_e) => Text::styled(
                                        "couldn't read link",
                                        Style::default().fg(Color::Gray),
                                    ),
                                }
                            } else if file.File.is_file() {
                                // check if file is a text readable
                                let mut prev_file = File::open(file.File.clone()).unwrap();
                                let mut prev_buffer = [0; 1024];
                                let bytes_read = prev_file.read(&mut prev_buffer[..]).unwrap();

                                if std::str::from_utf8(&prev_buffer[..bytes_read]).is_err() {
                                    Text::styled("binary file", Style::default().fg(Color::Gray))
                                } else {
                                    let prev_reader = BufReader::new(prev_file);
                                    let prev_content = if prev_reader.lines().count() == 0 {
                                        "empty file".to_string()
                                    } else {
                                        let prev_file = File::open(file.File.clone()).unwrap();
                                        let prev_reader = BufReader::new(prev_file);
                                        let mut content_buff = "".to_string();
                                        for line in
                                            prev_reader.lines().take(preview_height as usize)
                                        {
                                            match line {
                                                Ok(v) => {
                                                    content_buff = format!("{content_buff}{v}\n")
                                                }
                                                Err(_) => {}
                                            }
                                        }
                                        content_buff
                                    };

                                    Text::styled(prev_content, Style::default().fg(Color::Gray))
                                }
                            } else {
                                Text::styled("unknown file type", Style::default().fg(Color::Gray))
                            };

                            let fg_color: Color = if file.File.is_dir() {
                                SELECTED_FG_COLOR_DIR
                            } else if file.File.is_symlink() {
                                SELECTED_FG_COLOR_LINK
                            } else {
                                SELECTED_FG_COLOR_FILE
                            };

                            Span::styled(
                                padded_str,
                                Style::default()
                                    .bg(SELECTED_BG_COLOR)
                                    .fg(fg_color)
                                    .add_modifier(Modifier::BOLD),
                            )
                        } else {
                            let fg_color: Color = if file.File.is_dir() {
                                UNSELECTED_FG_COLOR_DIR
                            } else if file.File.is_symlink() {
                                UNSELECTED_FG_COLOR_LINK
                            } else {
                                UNSELECTED_FG_COLOR_FILE
                            };
                            Span::styled(original_file_name, Style::default().fg(fg_color))
                        };
                        ListItem::new(entry)
                    })
                    .collect();

                let list = List::new(list_items)
                    .block(Block::default().borders(Borders::ALL).title("Trash"))
                    .highlight_style(Style::default().fg(Color::Yellow));

                // ============= description
                let desc_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                    .split(midsection_chunks[1]);

                // -------------------- description
                let desc_block = Block::default()
                    .title("Description")
                    .borders(Borders::ALL)
                    .style(Style::default());
                let desc_text = Paragraph::new(selected_desc)
                    .wrap(Wrap { trim: false })
                    .block(desc_block);

                // -------------------- preview
                let preview_block = Block::default()
                    .title("Preview")
                    .borders(Borders::ALL)
                    .style(Style::default());
                let preview_text = Paragraph::new(preview)
                    .wrap(Wrap { trim: false })
                    .block(preview_block);

                f.render_widget(list, midsection_chunks[0]);
                f.render_widget(desc_text, desc_chunks[0]);
                f.render_widget(preview_text, desc_chunks[1]);
            }
            AppState::DeletionConfirmation(choice) => {
                let yes_no = if choice == 0 {
                    ("[Yes]", "No")
                } else {
                    ("Yes", "[No]")
                };

                let selected_file = &self.trashed_files[self.selected];
                let text = format!(
                    "Are you sure you want to restore '{}' to '{}'\n{}   {}",
                    selected_file
                        .OriginalFile
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    selected_file.OriginalFile.display().to_string(),
                    yes_no.0,
                    yes_no.1
                );
                // f.render_widget(dialog, chunks[1]);
                let area = f.area();
                let block = Block::bordered()
                    .title("Confirm Action")
                    .style(Style::default().bg(Color::Gray).fg(Color::Black));
                let area = popup_area(area, 40, 10);
                let dialog = Paragraph::new(text)
                    .style(Style::default().add_modifier(Modifier::BOLD))
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area); //this clears out the background
                f.render_widget(dialog, area);
            }
            _ => {}
        }

        // ================== footer
        let footer_block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default());

        let directions = Paragraph::new(Text::styled(
            "up/k - nav up, down/j - nav down, q - exit",
            Style::default(),
        ))
        .block(footer_block);

        f.render_widget(directions, chunks[2]);
    }

    fn handle_input(&mut self, key: KeyCode) {
        match self.state {
            AppState::MainScreen => {
                match key {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if self.selected > 0 {
                            self.selected -= 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if self.selected < self.trashed_files.len() - 1 {
                            self.selected += 1;
                        }
                    }
                    KeyCode::Enter => {
                        // let selected_file = &self.trashed_files[self.selected];
                        // println!(
                        //     "Executing action on: {}",
                        //     selected_file
                        //         .OriginalFile
                        //         .file_name()
                        //         .unwrap()
                        //         .to_str()
                        //         .unwrap()
                        // );
                        self.state = AppState::DeletionConfirmation(0);
                    }
                    KeyCode::Char('r') => {
                        // todo: refresh file list
                        todo!()
                    }
                    _ => {}
                }
            }
            AppState::DeletionConfirmation(choice) => {
                match key {
                    KeyCode::Left | KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('h') => {
                        // Toggle between Yes (0) and No (1)
                        if let AppState::DeletionConfirmation(choice) = &mut self.state {
                            *choice = if *choice == 0 { 1 } else { 0 };
                        }
                    }
                    KeyCode::Enter => {
                        // Confirm the action if Yes is selected
                        if let AppState::DeletionConfirmation(choice) = self.state {
                            if choice == 0 {
                                // Execute action on the selected file
                                let selected_file = &self.trashed_files[self.selected];
                                // println!("Performing action on: {:?}", selected_file.);
                            }
                        }
                        // Return to file list after action or cancel
                        self.state = AppState::MainScreen;
                    }
                    KeyCode::Esc => {
                        // Close the dialog without performing any action
                        self.state = AppState::MainScreen;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut files: Vec<TrashedFile> = vec![];

    // get user trash directory
    let user_home = get_home_dir().expect("couldn't get user home directory");
    let user_trash_dir = TrashDirectory::resolve_for_file(&user_home, VERBOSE_MODE)
        .expect("couldn't resolve user home trash dir");

    // iterate through entries in files and read the matching trashinfo, show the filename based on the entry
    // in trashinfo
    let mut home_files: Vec<TrashedFile> =
        get_trashed_files(user_trash_dir).expect("error while iterating trash files");
    files.append(&mut home_files);
    //
    // todo: do the same for every mounted drive

    // Setup terminal
    enable_raw_mode();
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;

    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(files);

    loop {
        terminal.draw(|f| app.render(f))?;

        // Handle input events
        if let Event::Key(key) = event::read()? {
            if key.kind == event::KeyEventKind::Release {
                // Skip events that are not KeyEventKind::Press
                continue;
            }

            if key.code == KeyCode::Char('q') {
                break;
            }

            app.handle_input(key.code)
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
    )?;
    terminal.show_cursor()?;

    ratatui::restore();
    Ok(())
}

fn get_trashed_files(trash_dir: TrashDirectory) -> Result<Vec<TrashedFile>, Box<dyn Error>> {
    let files_dir = trash_dir.files;
    let trashinfo_dir = trash_dir.info;

    let mut files: Vec<TrashedFile> = vec![];

    for child in read_dir(files_dir)? {
        let child = child?;
        let child_path = child.path();
        // println!("file {}", child_path.display());
        let trash_info_entry = trashinfo_dir.join(format!(
            "{}.trashinfo",
            child_path.file_name().unwrap().to_str().unwrap()
        ));
        // println!("checking {}", trash_info_entry.display());
        if !trash_info_entry.is_file() {
            // println!("{} is not a file", trash_info_entry.display());
            continue;
        }

        // println!("reading");
        let trashinfo_content =
            read_to_string(trash_info_entry).expect("couldn't read trashinfo entry");
        // println!("read:{}", trashinfo_content);
        let (original_path, deletion_date) = parse_trashinfo(&trashinfo_content)?;
        let original_file = PathBuf::from(&original_path);
        let trashed_entry = TrashedFile {
            OriginalFile: original_file,
            DeletionDate: deletion_date,
            File: child_path,
        };
        files.push(trashed_entry);
    }

    Ok(files)
}

fn parse_trashinfo(content: &str) -> Result<(String, String), Box<dyn Error>> {
    let lines: Vec<&str> = content.split("\n").collect();
    // println!("lines: {:?}", lines);
    if lines[0].trim() != "[Trash Info]"
        || !lines[1].starts_with("Path=")
        || !lines[2].starts_with("DeletionDate=")
    {
        return Err(Box::<dyn Error>::from("not a valid trashinfo entry"));
    }

    let original_path = &lines[1]["Path=".len()..];
    let original_path = decode(original_path).expect("utf-8").into_owned();
    let deletion_date = &lines[2]["DeletionDate=".len()..];
    // println!("{original_path}, {deletion_date}");

    Ok((original_path, deletion_date.to_string()))
}

/// helper function to create a centered rect using up certain percentage of the available rect `r`
fn popup_area(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)]).flex(Flex::Center);
    let horizontal = Layout::horizontal([Constraint::Percentage(percent_x)]).flex(Flex::Center);
    let [area] = vertical.areas(area);
    let [area] = horizontal.areas(area);
    area
}
