use std::error::Error;
use std::fs::read_dir;

use crossterm::event::KeyModifiers;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
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
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::{restore, Frame, Terminal};
use std::cmp::Ordering::{Equal, Greater, Less};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::MAIN_SEPARATOR_STR;
use std::str::from_utf8;

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

// how many items on each side before scrolling starts
const FILELIST_SCROLL_VIEW_OFFSET: usize = 3;

// todo - empty trash bin function
//  1. empty all trash - may error out because of permissions
//  2. empty home trash - sure fire
// todo: filter by
//  - root type
//  - large files
//  - last 7 days
// todo: find (fuzzy) by name, path, origin
// todo: open file with default viewer
// todo: show a message of confirmation/failure
// todo: show help on f1 with shortcuts
// todo: message if trash bin is empty

#[derive(Clone, Copy, PartialEq)]
enum SortType {
    DeletionDate,
    TrashRoot,
    Size,
    FileName,
    // FileType,
}

#[derive(PartialEq)]
enum AppState {
    RefreshFileList,
    MainScreen,
    RestoreConfirmation(usize),
    DeletionConfirmation(usize),
    EmptyBinConfirmation(usize),
    SortListDialog(SortType),
    Exiting,
}

struct App {
    state: AppState,
    trashed_files: Vec<TrashFile>,
    selected: usize,
    sort_type: SortType,
    scroll_offset: usize,
    max_visible_items: usize,
}

impl App {
    fn new() -> Self {
        Self {
            state: AppState::RefreshFileList,
            trashed_files: vec![],
            selected: 0,
            sort_type: SortType::DeletionDate,
            scroll_offset: 0,
            max_visible_items: 0,
        }
    }

    fn render(&mut self, f: &mut Frame) {
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

        let frame_area = f.area();
        let title = "Trash Bin";
        let padding = (frame_area.width as usize).saturating_sub(title.len());
        let padded_title = format!("{}{}", title, " ".repeat(padding));
        let title = Paragraph::new(Text::styled(
            padded_title,
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::Green)
                .fg(Color::Black),
        ))
        .block(title_block);

        f.render_widget(title, chunks[0]);

        let mut directions: Line = Line::default();

        // ================== mid section
        match &self.state {
            AppState::MainScreen => {
                let midsection_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
                    .split(chunks[1]);

                // file list area details
                // 60% of the screen width
                let file_list_width = (frame_area.width as f32 * 0.6).ceil() as usize;
                // -2 for left and right border
                // let file_list_inner_width = file_list_width - 2;
                // -2 for the border on top and bottom
                let file_list_height =
                    (frame_area.height - TITLE_HEIGHT - FOOTER_HEIGHT - 2) as usize;
                self.max_visible_items = file_list_height;
                // +1 for the left border
                // let file_list_inner_x = 1;
                // +1 for the top border
                // let file_list_inner_y = TITLE_HEIGHT + 1;

                let mut selected_desc: Text = Text::default();
                let mut preview: Text = Text::default();
                let preview_height: i32 =
                    ((frame_area.height - TITLE_HEIGHT - FOOTER_HEIGHT) as f32 * 0.6).floor()
                        as i32; // 60% of the midsection height
                let scroll_end =
                    (self.scroll_offset + self.max_visible_items).min(self.trashed_files.len());

                // ================= file list
                let list_items: Vec<ListItem> = self.trashed_files[self.scroll_offset..scroll_end]
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

                        // selection highlight padding
                        let selection_padding =
                            (file_list_width as usize).saturating_sub(original_file_name.len());
                        let padded_str =
                            format!("{}{}", original_file_name, " ".repeat(selection_padding));

                        // checking if current item is the selected needs to
                        // include the scroll offset
                        let entry = if i == (self.selected - self.scroll_offset) {
                            // generate description
                            let f_size = file.get_size().expect("error while getting file size");
                            let f_size_display = if f_size <= 1000 {
                                format!("{f_size}B")
                            } else if f_size <= 1000000 {
                                format!("{}KB", f_size / 1000)
                            } else {
                                format!("{}MB", f_size / 1000000)
                            };

                            // absolute paths are available only for the
                            // trashed files in the user's home
                            let original_path = match file.trashroot.root_type {
                                TrashRootType::Home => file.original_file.display().to_string(),
                                _ => format!(
                                    "{}{}{}",
                                    file.trashroot.home.parent().unwrap().display().to_string(),
                                    MAIN_SEPARATOR_STR,
                                    file.original_file.to_str().unwrap()
                                ),
                            };

                            let f_type: String = if file.files_entry.as_ref().unwrap().is_symlink()
                            {
                                "Link".to_string()
                            } else if file.files_entry.as_ref().unwrap().is_dir() {
                                "Directory".to_string()
                            } else {
                                "File".to_string()
                            };

                            selected_desc = Text::from(vec![
                                Line::from(vec![
                                    Span::styled(
                                        "File Type: ",
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
                                        "File Size: ",
                                        Style::default().add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(f_size_display, Style::default().fg(Color::Gray)),
                                ]),
                            ]);

                            // generate file preview
                            preview = if file.files_entry.as_ref().unwrap().is_symlink() {
                                match fs::read_link(file.files_entry.as_ref().unwrap().clone()) {
                                    Ok(target_path) => {
                                        let target_path_str =
                                            target_path.to_string_lossy().to_string();
                                        Text::from(vec![Line::from(vec![
                                            Span::styled(
                                                "Original target: ",
                                                Style::default()
                                                    .add_modifier(Modifier::BOLD)
                                                    .fg(Color::DarkGray),
                                            ),
                                            Span::styled(
                                                target_path_str,
                                                Style::default().fg(Color::White),
                                            ),
                                        ])])
                                    }
                                    Err(_e) => Text::styled(
                                        "couldn't read link",
                                        Style::default().fg(Color::LightRed),
                                    ),
                                }
                            } else if file.files_entry.as_ref().unwrap().is_dir() {
                                // show contents up to preview_height
                                let mut lines = vec![];
                                let entries = read_dir(file.files_entry.as_ref().unwrap().clone())
                                    .unwrap()
                                    .map(|res| res.map(|e| e.path()))
                                    .collect::<Result<Vec<_>, io::Error>>()
                                    .unwrap();

                                let item_count = entries.len();
                                if item_count == 0 {
                                    lines.push(Line::from(vec![Span::styled(
                                        "empty directory",
                                        Style::default().fg(Color::DarkGray),
                                    )]));
                                } else {
                                    lines.push(Line::from("."));
                                    for (i, entry) in entries.into_iter().enumerate() {
                                        if i > preview_height as usize {
                                            break;
                                        }

                                        let indicator = if i + 1 < item_count {
                                            Span::styled("├── ", Style::default())
                                        } else {
                                            Span::styled("└── ", Style::default())
                                        };
                                        let item = if entry.is_symlink() {
                                            Span::styled(
                                                entry
                                                    .file_name()
                                                    .unwrap()
                                                    .to_os_string()
                                                    .into_string()
                                                    .unwrap(),
                                                Style::default().fg(UNSELECTED_FG_COLOR_LINK),
                                            )
                                        } else if entry.is_dir() {
                                            Span::styled(
                                                entry
                                                    .file_name()
                                                    .unwrap()
                                                    .to_os_string()
                                                    .into_string()
                                                    .unwrap(),
                                                Style::default().fg(UNSELECTED_FG_COLOR_DIR),
                                            )
                                        } else {
                                            Span::styled(
                                                entry
                                                    .file_name()
                                                    .unwrap()
                                                    .to_os_string()
                                                    .into_string()
                                                    .unwrap(),
                                                Style::default().fg(UNSELECTED_FG_COLOR_FILE),
                                            )
                                        };
                                        lines.push(Line::from(vec![indicator, item]));
                                    }
                                }
                                Text::from(lines)
                            } else if file.files_entry.as_ref().unwrap().is_file() {
                                if file.get_size().ok().unwrap() == 0 {
                                    Text::styled(
                                        "empty file".to_string(),
                                        Style::default().fg(Color::DarkGray),
                                    )
                                } else {
                                    // check if file is a text readable
                                    let prev_file =
                                        File::open(file.files_entry.as_ref().unwrap().clone())
                                            .unwrap();
                                    let mut text_checker_reader = BufReader::new(&prev_file);
                                    let mut text_checker_line = vec![];
                                    let bytes_read = match text_checker_reader
                                        .read_until(b'\n', &mut text_checker_line)
                                    {
                                        Ok(v) => v,
                                        Err(_) => 0,
                                    };

                                    if bytes_read == 0 {
                                        Text::styled(
                                            "binary file",
                                            Style::default().fg(Color::DarkGray),
                                        )
                                    } else {
                                        let test_line_read =
                                            from_utf8(&text_checker_line[..bytes_read]);
                                        if test_line_read.is_err() || test_line_read.ok().is_none()
                                        {
                                            Text::styled(
                                                "binary file",
                                                Style::default().fg(Color::DarkGray),
                                            )
                                        } else {
                                            // read at most 15 lines
                                            let prev_file = File::open(
                                                file.files_entry.as_ref().unwrap().clone(),
                                            )
                                            .unwrap();
                                            let mut prev_reader = BufReader::new(prev_file);
                                            let mut bytes_total: usize = 0;
                                            let mut line_buff: Vec<u8> = vec![];
                                            let mut eof_reached = false;
                                            for _ in 1..preview_height.min(15) {
                                                let bytes_read = match prev_reader
                                                    .read_until(b'\n', &mut line_buff)
                                                {
                                                    Ok(v) => v,
                                                    Err(_) => 0,
                                                };

                                                // EOF
                                                if bytes_read == 0 {
                                                    eof_reached = true;
                                                    break;
                                                }

                                                bytes_total += bytes_read;
                                            }

                                            // some files could be non-text even
                                            // though the first line is textual
                                            match from_utf8(&line_buff[..bytes_total]) {
                                                Ok(v) => {
                                                    let mut content = v.to_owned();
                                                    if !eof_reached {
                                                        content.push_str("\n...");
                                                    }
                                                    Text::styled(
                                                        content,
                                                        Style::default().fg(Color::Gray),
                                                    )
                                                }
                                                Err(_) => Text::styled(
                                                    "binary file",
                                                    Style::default().fg(Color::DarkGray),
                                                ),
                                            }
                                        }
                                    }
                                }
                            } else {
                                Text::styled(
                                    "unknown file type",
                                    Style::default().fg(Color::DarkGray),
                                )
                            };

                            // generate list item entry
                            let (fg_color, entry_filetype) =
                                if file.files_entry.as_ref().unwrap().is_symlink() {
                                    (SELECTED_FG_COLOR_LINK, Span::from("🔗"))
                                } else if file.files_entry.as_ref().unwrap().is_dir() {
                                    (SELECTED_FG_COLOR_DIR, Span::from("📁"))
                                } else {
                                    (SELECTED_FG_COLOR_FILE, Span::from("📄"))
                                };

                            let entry_text = Span::styled(
                                padded_str,
                                Style::default()
                                    .bg(SELECTED_BG_COLOR)
                                    .fg(fg_color)
                                    .add_modifier(Modifier::BOLD),
                            );

                            let entry_symbol = match file.trashroot.root_type {
                                TrashRootType::Home => Span::from("  "),
                                // _ => Span::from("💾"),
                                _ => Span::from("🢅 "),
                            };

                            Line::from(vec![entry_symbol, entry_filetype, entry_text])
                        } else {
                            let (fg_color, entry_filetype) =
                                if file.files_entry.as_ref().unwrap().is_symlink() {
                                    (UNSELECTED_FG_COLOR_LINK, Span::from("🔗"))
                                } else if file.files_entry.as_ref().unwrap().is_dir() {
                                    (UNSELECTED_FG_COLOR_DIR, Span::from("📁"))
                                } else {
                                    (UNSELECTED_FG_COLOR_FILE, Span::from("📄"))
                                };
                            let entry_text =
                                Span::styled(original_file_name, Style::default().fg(fg_color));
                            let entry_symbol = match file.trashroot.root_type {
                                TrashRootType::Home => Span::from("  "),
                                // _ => Span::from("💾"),
                                _ => Span::from("🢅 "),
                            };
                            Line::from(vec![entry_symbol, entry_filetype, entry_text])
                        };

                        ListItem::new(entry)
                    })
                    .collect();

                let total_item_count = self.trashed_files.len();
                let list = List::new(list_items)
                    .block(
                        Block::default().borders(Borders::ALL).title(Span::styled(
                            format!(
                                "Files in Trash [{}/{}]",
                                self.selected + 1,
                                total_item_count,
                            ),
                            Style::default()
                                .bg(Color::Green)
                                .fg(Color::Black)
                                .add_modifier(Modifier::BOLD),
                        )),
                    )
                    .highlight_style(Style::default().fg(Color::Yellow));

                // ============= right column
                let desc_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                    .split(midsection_chunks[1]);

                // -------------------- description
                let desc_block = Block::default()
                    .title(Span::styled(
                        "Description",
                        Style::default()
                            .bg(Color::Green)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .style(Style::default());
                let desc_text = Paragraph::new(selected_desc)
                    .wrap(Wrap { trim: false })
                    .block(desc_block);

                // -------------------- preview
                let preview_block = Block::default()
                    .title(Span::styled(
                        "Preview",
                        Style::default()
                            .bg(Color::Green)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .style(Style::default());
                let preview_text = Paragraph::new(preview)
                    .wrap(Wrap { trim: false })
                    .block(preview_block);

                // -------------------- shortcuts
                let sort_value = match self.sort_type {
                    SortType::DeletionDate => "[Deleted On]",
                    SortType::TrashRoot => "[Origin]",
                    SortType::Size => "[Size]",
                    SortType::FileName => "[File Name]",
                    // SortType::FileType => "[File Type]",
                };
                directions = Line::from(vec![
                    Span::styled(
                        "up/k",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" Nav up, ", Style::default()),
                    Span::styled(
                        "down/j",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" Nav down, ", Style::default()),
                    Span::styled(
                        "enter",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" Restore, ", Style::default()),
                    Span::styled(
                        "q",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" Quit, ", Style::default()),
                    Span::styled(
                        "s",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" Sort By", Style::default()),
                    Span::styled(
                        sort_value,
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Yellow)
                            .fg(Color::Black),
                    ),
                    Span::styled(", ", Style::default()),
                    Span::styled(
                        "r/F5",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" Refersh, ", Style::default()),
                    Span::styled(
                        "g/PageUp",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" Go to top, ", Style::default()),
                    Span::styled(
                        "G/PageDown",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" Go to bottom", Style::default()),
                ]);

                f.render_widget(list, midsection_chunks[0]);
                f.render_widget(desc_text, desc_chunks[0]);
                f.render_widget(preview_text, desc_chunks[1]);

                // scroll bar for the list
                let scrollbar = if total_item_count <= self.max_visible_items {
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .thumb_symbol("░")
                        .track_symbol(Some("░"))
                } else {
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .thumb_symbol("█")
                        .track_symbol(Some("░"))
                };
                let mut scrollbar_state =
                    ScrollbarState::new(total_item_count).position(self.selected);
                f.render_stateful_widget(scrollbar, midsection_chunks[0], &mut scrollbar_state);
            }

            AppState::RestoreConfirmation(choice) => {
                // question in some mixed style
                let selected_file = &self.trashed_files[self.selected];
                let question = Line::from(vec![
                    Span::styled("This will restore ", Style::default()),
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
                    .title("Confirm Restoration")
                    .style(Style::default().bg(Color::Gray).fg(Color::Black));
                let area = popup_area(area, 40, 15);
                let dialog = Paragraph::new(vec![question, Line::from(vec![]), buttons])
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area); //this clears out the background
                f.render_widget(dialog, area);

                directions = Line::from(vec![
                    Span::styled(
                        "left/right h/l",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" select, ", Style::default()),
                    Span::styled(
                        "enter",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" confirm selection, ", Style::default()),
                    Span::styled(
                        "q/esc",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" cancel, ", Style::default()),
                ]);
            }

            AppState::DeletionConfirmation(choice) => {
                // question in some mixed style
                let selected_file = &self.trashed_files[self.selected];
                let question = Line::from(vec![
                    Span::styled("This will permanently delete ", Style::default()),
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
                    Span::styled(" forever?", Style::default()),
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
                    .title("Confirm Deletion")
                    .style(Style::default().bg(Color::Gray).fg(Color::Black));
                let area = popup_area(area, 40, 15);
                let dialog = Paragraph::new(vec![question, Line::from(vec![]), buttons])
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area); //this clears out the background
                f.render_widget(dialog, area);

                directions = Line::from(vec![
                    Span::styled(
                        "left/right h/l",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" select, ", Style::default()),
                    Span::styled(
                        "enter",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" confirm selection, ", Style::default()),
                    Span::styled(
                        "q/esc",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" cancel, ", Style::default()),
                ]);
            }

            AppState::EmptyBinConfirmation(choice) => {
                // question in some mixed style
                // let selected_file = &self.trashed_files[self.selected];
                let question = Line::from(vec![Span::styled(
                    "This will permanently delete ALL files in the trash bin forever",
                    Style::default(),
                )]);

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
                    .title("Confirm Empty Bin")
                    .style(Style::default().bg(Color::Gray).fg(Color::Black));
                let area = popup_area(area, 30, 10);
                let dialog = Paragraph::new(vec![question, Line::from(vec![]), buttons])
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area); //this clears out the background
                f.render_widget(dialog, area);

                directions = Line::from(vec![
                    Span::styled(
                        "left/right h/l",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" select, ", Style::default()),
                    Span::styled(
                        "enter",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" confirm selection, ", Style::default()),
                    Span::styled(
                        "q/esc",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" cancel, ", Style::default()),
                ]);
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

                // file name
                let fn_check_mark = if self.sort_type == SortType::FileName {
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

                let fn_label = if *choice == SortType::FileName {
                    Span::styled(
                        " File Name ",
                        Style::default().bg(Color::Black).fg(Color::White),
                    )
                } else {
                    Span::styled(" File Name ", Style::default())
                };

                choices.push(Line::from(vec![fn_check_mark, fn_label]));

                // popup dialog
                let mut dialog_content = vec![question, Line::from(vec![])];
                dialog_content.append(&mut choices);

                let area = f.area();
                let block = Block::bordered()
                    .title("Sort files by")
                    .style(Style::default().bg(Color::Gray).fg(Color::Black));
                let area = popup_area(area, 30, 15);
                let dialog = Paragraph::new(dialog_content)
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area); //this clears out the background
                f.render_widget(dialog, area);

                directions = Line::from(vec![
                    Span::styled(
                        "up/down k/j",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" select, ", Style::default()),
                    Span::styled(
                        "enter",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" confirm selection, ", Style::default()),
                    Span::styled(
                        "q/esc",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Green)
                            .fg(Color::Black),
                    ),
                    Span::styled(" cancel, ", Style::default()),
                ]);
            }
            _ => {}
        }

        // ================== footer
        let footer_block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default());

        let directions_block = Paragraph::new(directions).block(footer_block);

        f.render_widget(directions_block, chunks[2]);
    }

    fn handle_input(&mut self, key: KeyEvent) {
        match self.state {
            AppState::MainScreen => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.selected > 0 {
                        self.selected -= 1;
                    }

                    // if selected item is close to a position to start scrolling up
                    // unless it's close to the top
                    if self.selected >= FILELIST_SCROLL_VIEW_OFFSET
                        && self.selected - FILELIST_SCROLL_VIEW_OFFSET < self.scroll_offset
                    {
                        self.scroll_offset = self.selected - FILELIST_SCROLL_VIEW_OFFSET;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.selected < self.trashed_files.len() - 1 {
                        self.selected += 1;
                    }

                    // if the selected item is close to a position to start scrolling down
                    // unless it results in overscrolling
                    if self.selected + FILELIST_SCROLL_VIEW_OFFSET < self.trashed_files.len()
                        && self.selected + FILELIST_SCROLL_VIEW_OFFSET
                            >= self.scroll_offset + self.max_visible_items
                    {
                        // since scroll_offset is usize, need to offset first before substracting
                        self.scroll_offset = self.selected + FILELIST_SCROLL_VIEW_OFFSET
                            - self.max_visible_items
                            + 1;
                    }
                }
                KeyCode::Enter => {
                    self.state = AppState::RestoreConfirmation(0);
                }
                KeyCode::Delete => {
                    if key.modifiers == KeyModifiers::SHIFT {
                        // empty bin
                        self.state = AppState::EmptyBinConfirmation(0);
                    } else {
                        // delete selected file
                        self.state = AppState::DeletionConfirmation(0);
                    }
                }
                KeyCode::Char('r') | KeyCode::F(5) => {
                    self.state = AppState::RefreshFileList;
                }
                KeyCode::Char('s') => {
                    self.state = AppState::SortListDialog(self.sort_type);
                }
                KeyCode::Char('g') | KeyCode::PageUp => {
                    self.selected = 0;
                    self.scroll_offset = 0;
                }
                KeyCode::Char('G') | KeyCode::PageDown => {
                    self.selected = self.trashed_files.len() - 1;
                    self.scroll_offset = self.selected - self.max_visible_items + 1;
                }
                KeyCode::Char('q') => {
                    self.state = AppState::Exiting;
                }
                _ => {}
            },

            AppState::RestoreConfirmation(choice) => {
                match key.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('h') => {
                        // Toggle between Yes (0) and No (1)
                        if let AppState::RestoreConfirmation(choice) = &mut self.state {
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

            AppState::DeletionConfirmation(choice) => {
                match key.code {
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
                            let _ = selected_file
                                .delete_forever()
                                .expect("could not delete file");
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

            AppState::EmptyBinConfirmation(choice) => {
                match key.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('h') => {
                        // Toggle between Yes (0) and No (1)
                        if let AppState::DeletionConfirmation(choice) = &mut self.state {
                            *choice = if *choice == 0 { 1 } else { 0 };
                        }
                    }
                    KeyCode::Enter => {
                        // Confirm the action if Yes is selected
                        if choice == 0 {
                            for trash_file in &self.trashed_files {
                                // one error shouldn't stop operation
                                // TODO: DO NOT TEST THIS BEFORE FIXING THE .desktop BUG
                                match trash_file.delete_forever() {
                                    Ok(_) => {}
                                    Err(_) => {} // todo: show an error notification
                                }
                            }
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

            AppState::SortListDialog(choice) => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    let next_choice = match choice {
                        SortType::DeletionDate => SortType::TrashRoot,
                        SortType::TrashRoot => SortType::Size,
                        SortType::Size => SortType::FileName,
                        SortType::FileName => SortType::FileName,
                    };
                    self.state = AppState::SortListDialog(next_choice);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let prev_choice = match choice {
                        SortType::DeletionDate => SortType::DeletionDate,
                        SortType::TrashRoot => SortType::DeletionDate,
                        SortType::Size => SortType::TrashRoot,
                        SortType::FileName => SortType::Size,
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
                app.trashed_files = get_trashed_files()?;
                sort_file_list(&mut app.trashed_files, &app.sort_type);
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

            app.handle_input(key)
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

fn get_trashed_files() -> Result<Vec<TrashFile>, Box<dyn Error>> {
    // get user trash directory
    let user_home = get_home_dir().expect("couldn't get user home directory");
    let user_trash_dir = TrashDirectory::resolve_for_file(&user_home, VERBOSE_MODE)
        .expect("couldn't resolve user home trash dir");

    // get all trash locations currently mounted
    let mut trash_roots: Vec<TrashDirectory> = TrashDirectory::get_all_trash_roots()?;
    trash_roots.push(user_trash_dir);

    let mut files: Vec<TrashFile> = vec![];
    for trash_root in trash_roots {
        let mut trash_files = trash_root.get_trashed_files()?;
        files.append(&mut trash_files);
    }

    Ok(files)
}

fn sort_file_list(list: &mut Vec<TrashFile>, sort_by: &SortType) {
    list.sort_by(|a, b| match sort_by {
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
        SortType::FileName => {
            let a_name = a.original_file.clone();
            let b_name = b.original_file.clone();
            a_name
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_lowercase()
                .cmp(&b_name.file_name().unwrap().to_str().unwrap().to_lowercase())
        }
    });
}

/// helper function to create a centered rect using up certain percentage of the available rect `r`
/// copied from ratatui docs
fn popup_area(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)]).flex(Flex::Center);
    let horizontal = Layout::horizontal([Constraint::Percentage(percent_x)]).flex(Flex::Center);
    let [area] = vertical.areas(area);
    let [area] = horizontal.areas(area);
    area
}
