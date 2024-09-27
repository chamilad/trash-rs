use std::error::Error;
use std::fs::read_dir;

use crossterm::event::{self, Event, KeyCode};
use libtrash::*;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::DisableMouseCapture;
use ratatui::crossterm::event::EnableMouseCapture;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{restore, Frame, Terminal};
use std::cmp::Ordering::{Equal, Greater, Less};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};

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

#[derive(Clone, Copy, PartialEq)]
enum SortType {
    DeletionDate,
    TrashRoot,
    Size,
}

#[derive(PartialEq)]
enum AppState {
    RefreshFileList,
    MainScreen,
    DeletionConfirmation(usize),
    SortListDialog(SortType),
    Exiting,
}

struct App {
    state: AppState,
    trashed_files: Vec<TrashFile>,
    selected: usize,
    sort_type: SortType,
}

impl App {
    fn new() -> Self {
        Self {
            state: AppState::RefreshFileList,
            trashed_files: vec![],
            selected: 0,
            sort_type: SortType::DeletionDate,
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
        match &self.state {
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
                            .original_file
                            .file_name()
                            .expect("file_name")
                            .to_os_string()
                            .into_string()
                            .unwrap();

                        let original_path = file.original_file.to_str().unwrap();

                        // Calculate padding to fill the remaining space for full line width
                        let padding =
                            (file_list_width as usize).saturating_sub(original_file_name.len());
                        let padded_str = format!("{}{}", original_file_name, " ".repeat(padding));

                        let f_type: String = if file.files_entry.as_ref().unwrap().is_dir() {
                            "Directory".to_string()
                        } else if file.files_entry.as_ref().unwrap().is_symlink() {
                            "Link".to_string()
                        } else {
                            "File".to_string()
                        };

                        let entry = if i == self.selected {
                            let f_size = file.get_size().expect("error while getting file size");
                            let f_size_display = if f_size <= 1000 {
                                format!("{f_size}B")
                            } else if f_size <= 1000000 {
                                format!("{}KB", f_size / 1000)
                            } else {
                                format!("{}MB", f_size / 1000000)
                            };
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
                                        file.trashinfo.as_ref().unwrap().deletion_date.clone(),
                                        Style::default().fg(Color::Gray),
                                    ),
                                ]),
                                Line::from(vec![
                                    Span::styled(
                                        "Size: ",
                                        Style::default().add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(f_size_display, Style::default().fg(Color::Gray)),
                                ]),
                            ]);

                            // generate file preview
                            preview = if file.files_entry.as_ref().unwrap().is_dir() {
                                // show contents up to 10
                                let mut lines = vec![];
                                let entries = read_dir(file.files_entry.as_ref().unwrap().clone())
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
                                        // todo: bug check for symlink before is_dir()
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
                            } else if file.files_entry.as_ref().unwrap().is_symlink() {
                                match fs::read_link(file.files_entry.as_ref().unwrap().clone()) {
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
                            } else if file.files_entry.as_ref().unwrap().is_file() {
                                // check if file is a text readable
                                let mut prev_file =
                                    File::open(file.files_entry.as_ref().unwrap().clone()).unwrap();
                                let mut prev_buffer = [0; 1024];
                                let bytes_read = prev_file.read(&mut prev_buffer[..]).unwrap();

                                if std::str::from_utf8(&prev_buffer[..bytes_read]).is_err() {
                                    Text::styled("binary file", Style::default().fg(Color::Gray))
                                } else {
                                    let prev_reader = BufReader::new(prev_file);
                                    let prev_content = if prev_reader.lines().count() == 0 {
                                        // todo: bug: .desktop file is marked as empty
                                        "empty file".to_string()
                                    } else {
                                        let prev_file =
                                            File::open(file.files_entry.as_ref().unwrap().clone())
                                                .unwrap();
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

                            let fg_color: Color = if file.files_entry.as_ref().unwrap().is_dir() {
                                SELECTED_FG_COLOR_DIR
                            } else if file.files_entry.as_ref().unwrap().is_symlink() {
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
                            let fg_color: Color = if file.files_entry.as_ref().unwrap().is_dir() {
                                UNSELECTED_FG_COLOR_DIR
                            } else if file.files_entry.as_ref().unwrap().is_symlink() {
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
                // question in some mixed style
                let selected_file = &self.trashed_files[self.selected];
                let question = Line::from(vec![
                    Span::styled("This will restore file ", Style::default()),
                    Span::styled(
                        format!(
                            "'{}' ",
                            selected_file
                                .original_file
                                .file_name()
                                .unwrap()
                                .to_str()
                                .unwrap(),
                        ),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("to ", Style::default()),
                    Span::styled(
                        format!("'{}' ", selected_file.original_file.display().to_string(),),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("?", Style::default()),
                ]);

                // space between buttons
                let spacer = Span::styled("      ", Style::default());

                // illusion of buttons
                let buttons = if *choice == 0 {
                    Line::from(vec![
                        Span::styled(
                            "[Confirm]",
                            Style::default()
                                .add_modifier(Modifier::BOLD)
                                .bg(Color::Black)
                                .fg(Color::White),
                        ),
                        spacer,
                        Span::styled("[Cancel]", Style::default()),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled("[Confirm]", Style::default()),
                        spacer,
                        Span::styled(
                            "[Cancel]",
                            Style::default()
                                .add_modifier(Modifier::BOLD)
                                .bg(Color::Black)
                                .fg(Color::White),
                        ),
                    ])
                };

                // popup dialog
                let area = f.area();
                let block = Block::bordered()
                    .title("Confirm Action")
                    .style(Style::default().bg(Color::Gray).fg(Color::Black));
                let area = popup_area(area, 40, 10);
                let dialog = Paragraph::new(vec![question, Line::from(vec![]), buttons])
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area); //this clears out the background
                f.render_widget(dialog, area);
            }

            AppState::SortListDialog(choice) => {
                let question = Line::from(vec![Span::styled(
                    "Select sort by column",
                    Style::default(),
                )]);

                let mut choices: Vec<Line> = vec![];
                // Deletion Date
                let dd_check_mark = if self.sort_type == SortType::DeletionDate {
                    Span::styled(
                        "[x]",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Black)
                            .fg(Color::White),
                    )
                } else {
                    Span::styled("[ ]", Style::default())
                };

                let dd_label = if *choice == SortType::DeletionDate {
                    Span::styled(
                        " Deleted on",
                        Style::default().bg(Color::Black).fg(Color::White),
                    )
                } else {
                    Span::styled(" Deleted on", Style::default())
                };

                choices.push(Line::from(vec![dd_check_mark, dd_label]));

                // Origin
                let o_check_mark = if self.sort_type == SortType::TrashRoot {
                    Span::styled(
                        "[x]",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Black)
                            .fg(Color::White),
                    )
                } else {
                    Span::styled("[ ]", Style::default())
                };

                let o_label = if *choice == SortType::TrashRoot {
                    Span::styled(
                        " Origin    ",
                        Style::default().bg(Color::Black).fg(Color::White),
                    )
                } else {
                    Span::styled(" Origin    ", Style::default())
                };

                choices.push(Line::from(vec![o_check_mark, o_label]));

                // Size
                let s_check_mark = if self.sort_type == SortType::Size {
                    Span::styled(
                        "[x]",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Black)
                            .fg(Color::White),
                    )
                } else {
                    Span::styled("[ ]", Style::default())
                };

                let s_label = if *choice == SortType::Size {
                    Span::styled(
                        " Size      ",
                        Style::default().bg(Color::Black).fg(Color::White),
                    )
                } else {
                    Span::styled(" Size      ", Style::default())
                };

                choices.push(Line::from(vec![s_check_mark, s_label]));

                // popup dialog
                let mut dialog_content = vec![question, Line::from(vec![])];
                dialog_content.append(&mut choices);

                let area = f.area();
                let block = Block::bordered()
                    .title("Sort files by")
                    .style(Style::default().bg(Color::Gray).fg(Color::Black));
                let area = popup_area(area, 30, 10);
                let dialog = Paragraph::new(dialog_content)
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
            AppState::MainScreen => match key {
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
                    self.state = AppState::DeletionConfirmation(0);
                }
                KeyCode::Char('r') => {
                    self.state = AppState::RefreshFileList;
                }
                KeyCode::Char('s') => {
                    self.state = AppState::SortListDialog(self.sort_type);
                }
                KeyCode::Char('g') => {
                    self.selected = 0;
                }
                KeyCode::Char('G') => {
                    self.selected = self.trashed_files.len() - 1;
                }
                KeyCode::Char('q') => {
                    self.state = AppState::Exiting;
                }
                _ => {}
            },

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
                        if choice == 0 {
                            let selected_file = &self.trashed_files[self.selected];
                            let _ = selected_file.restore().expect("could not restore file");
                        }

                        // Refresh and return to file list after action or cancel
                        self.state = AppState::RefreshFileList;
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // Close the dialog without performing any action
                        self.state = AppState::RefreshFileList;
                    }
                    _ => {}
                }
            }

            AppState::SortListDialog(choice) => match key {
                KeyCode::Down | KeyCode::Char('j') => {
                    let next_choice = match choice {
                        SortType::DeletionDate => SortType::TrashRoot,
                        SortType::TrashRoot => SortType::Size,
                        SortType::Size => SortType::Size,
                    };
                    self.state = AppState::SortListDialog(next_choice);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let prev_choice = match choice {
                        SortType::DeletionDate => SortType::DeletionDate,
                        SortType::TrashRoot => SortType::DeletionDate,
                        SortType::Size => SortType::TrashRoot,
                    };
                    self.state = AppState::SortListDialog(prev_choice);
                }
                KeyCode::Enter => {
                    self.sort_type = choice;
                    self.state = AppState::RefreshFileList;
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.state = AppState::RefreshFileList;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;

    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    loop {
        match app.state {
            AppState::RefreshFileList => {
                app.trashed_files = get_trashed_files(&app.sort_type)?;
                app.state = AppState::MainScreen;
            }
            AppState::Exiting => {
                break;
            }
            _ => {}
        }

        terminal.draw(|f| app.render(f))?;

        // Handle input events
        if let Event::Key(key) = event::read()? {
            if key.kind == event::KeyEventKind::Release {
                // Skip events that are not KeyEventKind::Press
                continue;
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

    restore();
    Ok(())
}

fn get_trashed_files(sort: &SortType) -> Result<Vec<TrashFile>, Box<dyn Error>> {
    // get user trash directory
    let user_home = get_home_dir().expect("couldn't get user home directory");
    let user_trash_dir = TrashDirectory::resolve_for_file(&user_home, VERBOSE_MODE)
        .expect("couldn't resolve user home trash dir");

    // iterate through entries in files and read the matching trashinfo, show the filename based on the entry
    // in trashinfo
    // let mut home_files: Vec<TrashedFile> =
    // get_trashed_files(user_trash_dir).expect("error while iterating trash files");
    //
    // todo: do the same for every mounted drive

    let mut files: Vec<TrashFile> = vec![];
    let mut home_trash_files = user_trash_dir.get_trashed_files()?;
    files.append(&mut home_trash_files);
    files.sort_by(|a, b| match sort {
        SortType::DeletionDate => {
            // sort by deletion date, if equal directories first
            let a_date = a.trashinfo.clone().unwrap().deletion_date;
            let b_date = b.trashinfo.clone().unwrap().deletion_date;
            let cmp_date = b_date.cmp(&a_date);

            // cmp_date
            match cmp_date {
                Equal => {
                    if a.files_entry.as_deref().unwrap().is_dir() {
                        Greater
                    } else {
                        Less
                    }
                }
                other => other,
            }
        }
        SortType::TrashRoot => {
            // compare by origin, if equal, then by deletion date
            let a_dev = a.trashroot.device.clone().dev_num.dev_id;
            let b_dev = b.trashroot.device.clone().dev_num.dev_id;
            let cmp_dev = a_dev.cmp(&b_dev);
            match cmp_dev {
                Equal => {
                    let a_date = a.trashinfo.clone().unwrap().deletion_date;
                    let b_date = b.trashinfo.clone().unwrap().deletion_date;
                    b_date.cmp(&a_date)
                }
                other => other,
            }
        }
        SortType::Size => {
            // compare by size, if equal, then by deletion date
            let a_size = a.get_size().expect("error while getting file size");
            let b_size = b.get_size().expect("error while getting file size");
            let cmp_size = b_size.cmp(&a_size);

            match cmp_size {
                Equal => {
                    let a_date = a.trashinfo.clone().unwrap().deletion_date;
                    let b_date = b.trashinfo.clone().unwrap().deletion_date;
                    b_date.cmp(&a_date)
                }
                other => other,
            }
        }
    });

    Ok(files)
}

/// helper function to create a centered rect using up certain percentage of the available rect `r`
fn popup_area(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)]).flex(Flex::Center);
    let horizontal = Layout::horizontal([Constraint::Percentage(percent_x)]).flex(Flex::Center);
    let [area] = vertical.areas(area);
    let [area] = horizontal.areas(area);
    area
}
