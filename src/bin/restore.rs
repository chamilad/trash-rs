use std::error::Error;
use std::fs::read_dir;

use chrono::Local;
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
    Block, Borders, Clear, List, ListItem, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::{restore, Frame, Terminal};
use std::cmp::Ordering::{Equal, Greater, Less};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::MAIN_SEPARATOR_STR;
use std::str::from_utf8;
use std::{env, usize};

const VERBOSE_MODE: bool = false;
const BINARY_NAME: &str = "Trash Bin";
const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");

// layout values
const LAYOUT_FILE_LIST_WIDTH_PERCENTAGE: u16 = 70;
const LAYOUT_PREVIEW_HEIGHT_PERCENTAGE: u16 = 70;
const LAYOUT_TITLE_HEIGHT: u16 = 3;
const LAYOUT_FOOTER_HEIGHT: u16 = 3;

// how many items on each side before scrolling starts
const FILELIST_SCROLL_VIEW_OFFSET: usize = 3;

// todo: filter by
//  - root type
//  - large files
//  - last 7 days
// todo: find (fuzzy) by name, path, origin
// todo: open file with default viewer
// todo: show a message of confirmation/failure

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
    HelpScreen,
    Exiting,
}

#[derive(PartialEq)]
enum Theme {
    Light,
    Dark,
}

enum ThemeColor {
    Highlight,
    TitleText,
    Text,
    BoldText,
    ErrorText,
    SelectedFGDir,
    SelectedFGLink,
    SelectedFGFile,
    SelectedBG,
    UnselectedFGDir,
    UnselectedFGLink,
    UnselectedFGFile,
    DialogBG,
    DialogText,
    DialogButtonBG,
    DialogButtonText,
}

struct App {
    state: AppState,
    trashed_files: Vec<TrashFile>,
    selected: usize,
    sort_type: SortType,
    scroll_offset: usize,
    max_visible_items: usize,
    theme: Theme,
}

impl App {
    fn new(theme: Theme) -> Self {
        Self {
            state: AppState::RefreshFileList,
            trashed_files: vec![],
            selected: 0,
            sort_type: SortType::DeletionDate,
            scroll_offset: 0,
            max_visible_items: 0,
            theme,
        }
    }

    fn render(&mut self, f: &mut Frame) {
        let title_style = Style::default()
            .add_modifier(Modifier::BOLD)
            .bg(self.get_color(ThemeColor::Highlight))
            .fg(self.get_color(ThemeColor::TitleText));

        let block_style = Style::default();
        let dialog_style = Style::default()
            .bg(self.get_color(ThemeColor::DialogBG))
            .fg(self.get_color(ThemeColor::DialogText));
        let dialog_text_style = Style::default().fg(self.get_color(ThemeColor::DialogText));
        let dialog_button_selected_style = Style::default()
            .add_modifier(Modifier::BOLD)
            .bg(self.get_color(ThemeColor::DialogButtonBG))
            .fg(self.get_color(ThemeColor::DialogButtonText));
        let dialog_button_unseleted_style = Style::default();

        let main_horizontal_blocks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(LAYOUT_TITLE_HEIGHT),
                    Constraint::Min(3),
                    Constraint::Length(LAYOUT_FOOTER_HEIGHT),
                ]
                .as_ref(),
            )
            .split(f.area());

        // ============================== title
        let title_block = Block::default().borders(Borders::ALL).style(block_style);

        let frame_area = f.area();
        let title = format!(" {BINARY_NAME}");
        let padded_title = format!("{title:<width$}", width = frame_area.width as usize);
        let title = Paragraph::new(Text::styled(padded_title, title_style)).block(title_block);
        f.render_widget(title, main_horizontal_blocks[0]);

        let mut directions: Line = Line::default();

        // ================== mid section
        match &self.state {
            AppState::MainScreen => {
                let midsection_columns = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(
                        [
                            Constraint::Percentage(LAYOUT_FILE_LIST_WIDTH_PERCENTAGE),
                            Constraint::Percentage(100 - LAYOUT_FILE_LIST_WIDTH_PERCENTAGE),
                        ]
                        .as_ref(),
                    )
                    .split(main_horizontal_blocks[1]);

                let total_item_count = self.trashed_files.len();
                let mut selected_desc: Text = Text::default();
                let mut preview: Text = Text::default();

                // if empty bin, show kitty
                if total_item_count == 0 {
                    let empty_note = r#"



          |\      _,,,---,,_
    ZZZzz /,`.-'`'    -.  ;-;;,_
         |,4-  ) )-,_. ,\ (  `'-'
        '---''(_/--'  `-'\_)
                        "#;
                    let note = Paragraph::new(empty_note).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(Span::styled(" Files in Trash [Empty] ", title_style)),
                    );
                    f.render_widget(note, midsection_columns[0]);
                } else {
                    // file list area details
                    let file_list_width = (frame_area.width as f32
                        * (LAYOUT_FILE_LIST_WIDTH_PERCENTAGE as f32 / 100.0))
                        .ceil() as usize;
                    let file_list_height =
                        (frame_area.height - LAYOUT_TITLE_HEIGHT - LAYOUT_FOOTER_HEIGHT - 2)
                            as usize; // -2 for the border on top bottom
                    self.max_visible_items = file_list_height;
                    let scroll_end =
                        (self.scroll_offset + self.max_visible_items).min(self.trashed_files.len());

                    // preview area details
                    let preview_area_height: usize =
                        ((frame_area.height - LAYOUT_TITLE_HEIGHT - LAYOUT_FOOTER_HEIGHT) as f32
                            * (LAYOUT_PREVIEW_HEIGHT_PERCENTAGE as f32 / 100.0))
                            .floor() as usize;
                    let preview_max_lines = preview_area_height - 5; // border top bottom + padding top bottom + indicator

                    // ================= file list
                    let list_items: Vec<ListItem> = self.trashed_files
                        [self.scroll_offset..scroll_end]
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

                            // checking if current item is the selected needs to
                            // include the scroll offset
                            let entry = if i == (self.selected - self.scroll_offset) {
                                // generate description
                                let f_size =
                                    file.get_size().expect("error while getting file size");
                                let f_size_display = if f_size <= 1000 {
                                    format!("{f_size}B")
                                } else if f_size <= 1000000 {
                                    format!("{}KB", f_size / 1000)
                                } else if f_size <= 1000000000 {
                                    format!("{}MB", f_size / 1000000)
                                } else {
                                    format!("{}GB", f_size / 1000000000)
                                };

                                // absolute paths are available only for the
                                // trashed files in the user's home
                                // also replace home with ~
                                let original_path_display = match file.trashroot.root_type {
                                    TrashRootType::Home => {
                                        match get_home_dir() {
                                            Ok(v) => file
                                                .original_file
                                                .display()
                                                .to_string()
                                                .replace(v.display().to_string().as_str(), "~"),
                                            Err(_) => file.original_file.display().to_string(),
                                        }
                                        // file.original_file.display().to_string()
                                    }
                                    _ => format!(
                                        "{}{}{}",
                                        file.trashroot.home.parent().unwrap().display(),
                                        MAIN_SEPARATOR_STR,
                                        file.original_file.to_str().unwrap()
                                    ),
                                };

                                let f_type: String =
                                    if file.files_entry.as_ref().unwrap().is_symlink() {
                                        "Link".to_string()
                                    } else if file.files_entry.as_ref().unwrap().is_dir() {
                                        "Directory".to_string()
                                    } else {
                                        "File".to_string()
                                    };

                                selected_desc = Text::from(vec![
                                    Line::from(vec![
                                        Span::styled(
                                            "Original path: ",
                                            Style::default()
                                                .fg(self.get_color(ThemeColor::BoldText))
                                                .add_modifier(Modifier::BOLD),
                                        ),
                                        Span::styled(
                                            original_path_display.clone(),
                                            Style::default().fg(self.get_color(ThemeColor::Text)),
                                        ),
                                    ]),
                                    Line::from(vec![
                                        Span::styled(
                                            "Deleted on: ",
                                            Style::default()
                                                .fg(self.get_color(ThemeColor::BoldText))
                                                .add_modifier(Modifier::BOLD),
                                        ),
                                        Span::styled(
                                            file.trashinfo.as_ref().unwrap().deletion_date.clone(),
                                            Style::default().fg(self.get_color(ThemeColor::Text)),
                                        ),
                                    ]),
                                    Line::from(vec![
                                        Span::styled(
                                            "File Type: ",
                                            Style::default()
                                                .fg(self.get_color(ThemeColor::BoldText))
                                                .add_modifier(Modifier::BOLD),
                                        ),
                                        Span::styled(
                                            f_type,
                                            Style::default().fg(self.get_color(ThemeColor::Text)),
                                        ),
                                    ]),
                                    Line::from(vec![
                                        Span::styled(
                                            "File Size: ",
                                            Style::default()
                                                .fg(self.get_color(ThemeColor::BoldText))
                                                .add_modifier(Modifier::BOLD),
                                        ),
                                        Span::styled(
                                            f_size_display.clone(),
                                            Style::default().fg(self.get_color(ThemeColor::Text)),
                                        ),
                                    ]),
                                ]);

                                // generate file preview
                                let message_style = Style::default()
                                    .fg(self.get_color(ThemeColor::Text))
                                    .add_modifier(Modifier::ITALIC);
                                let err_message_style = Style::default()
                                    .fg(self.get_color(ThemeColor::ErrorText))
                                    .add_modifier(Modifier::ITALIC);
                                preview = if file.files_entry.as_ref().unwrap().is_symlink() {
                                    match fs::read_link(file.files_entry.as_ref().unwrap().clone())
                                    {
                                        Ok(target_path) => {
                                            let target_path_str =
                                                target_path.to_string_lossy().to_string();
                                            Text::from(vec![Line::from(vec![
                                                Span::styled(
                                                    "original target: ",
                                                    Style::default()
                                                        .add_modifier(Modifier::BOLD)
                                                        .fg(self.get_color(ThemeColor::Text)),
                                                ),
                                                Span::styled(
                                                    target_path_str,
                                                    Style::default()
                                                        .fg(self.get_color(ThemeColor::BoldText)),
                                                ),
                                            ])])
                                        }
                                        Err(_e) => {
                                            Text::styled("couldn't read link", err_message_style)
                                        }
                                    }
                                } else if file.files_entry.as_ref().unwrap().is_dir() {
                                    // show contents up to preview_height
                                    let mut lines = vec![];
                                    let entries =
                                        read_dir(file.files_entry.as_ref().unwrap().clone())
                                            .unwrap()
                                            .map(|res| res.map(|e| e.path()))
                                            .collect::<Result<Vec<_>, io::Error>>()
                                            .unwrap();

                                    let item_count = entries.len();
                                    if item_count == 0 {
                                        lines.push(Line::from(vec![Span::styled(
                                            "empty directory",
                                            message_style,
                                        )]));
                                    } else {
                                        // show a tree -L 1 output
                                        lines.push(Line::styled(
                                            "directory contents",
                                            message_style,
                                        ));
                                        lines.push(Line::from("."));
                                        for (i, entry) in entries.into_iter().enumerate() {
                                            if i > preview_area_height {
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
                                                    Style::default().fg(self
                                                        .get_color(ThemeColor::UnselectedFGLink)),
                                                )
                                            } else if entry.is_dir() {
                                                Span::styled(
                                                    entry
                                                        .file_name()
                                                        .unwrap()
                                                        .to_os_string()
                                                        .into_string()
                                                        .unwrap(),
                                                    Style::default()
                                                        .fg(self
                                                            .get_color(ThemeColor::SelectedFGDir)),
                                                )
                                            } else {
                                                Span::styled(
                                                    entry
                                                        .file_name()
                                                        .unwrap()
                                                        .to_os_string()
                                                        .into_string()
                                                        .unwrap(),
                                                    Style::default().fg(self
                                                        .get_color(ThemeColor::UnselectedFGFile)),
                                                )
                                            };
                                            lines.push(Line::from(vec![indicator, item]));
                                        }
                                    }
                                    Text::from(lines)
                                } else if file.files_entry.as_ref().unwrap().is_file() {
                                    if file.get_size().ok().unwrap() == 0 {
                                        Text::styled("empty file", message_style)
                                    } else {
                                        // check if file is a text readable by
                                        // reading the first line (ending with \n)
                                        // and trying to parse it as utf-8
                                        // if this passes and another line fails later to parse,
                                        // that also counts as a binary file, since some "binary"
                                        // files could have textual headers
                                        let prev_file =
                                            File::open(file.files_entry.as_ref().unwrap().clone())
                                                .unwrap();
                                        let mut text_checker_reader = BufReader::new(&prev_file);
                                        let mut text_checker_line = vec![];
                                        let bytes_read = text_checker_reader
                                            .read_until(b'\n', &mut text_checker_line)
                                            .unwrap_or(0);

                                        if bytes_read == 0 {
                                            Text::styled("couldn't read file", err_message_style)
                                        } else {
                                            let test_line_read =
                                                from_utf8(&text_checker_line[..bytes_read]);
                                            if test_line_read.is_err()
                                                || test_line_read.ok().is_none()
                                            {
                                                Text::styled("binary file", message_style)
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
                                                for _ in
                                                    1..preview_area_height.min(preview_max_lines)
                                                {
                                                    let bytes_read = prev_reader
                                                        .read_until(b'\n', &mut line_buff)
                                                        .unwrap_or(0);

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
                                                            content.push_str("...\n...");
                                                        }
                                                        Text::styled(
                                                            content,
                                                            Style::default()
                                                                .fg(self
                                                                    .get_color(ThemeColor::Text)),
                                                        )
                                                    }
                                                    Err(_) => Text::styled(
                                                        "binary file",
                                                        Style::default()
                                                            .fg(self.get_color(ThemeColor::Text)),
                                                    ),
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    Text::styled("unknown file type", err_message_style)
                                };

                                // generate list item entry
                                let (fg_color, entry_filetype) = if file
                                    .files_entry
                                    .as_ref()
                                    .unwrap()
                                    .is_symlink()
                                {
                                    (self.get_color(ThemeColor::SelectedFGLink), Span::from("🔗"))
                                } else if file.files_entry.as_ref().unwrap().is_dir() {
                                    (self.get_color(ThemeColor::SelectedFGDir), Span::from("📁"))
                                } else {
                                    (self.get_color(ThemeColor::SelectedFGFile), Span::from("📄"))
                                };

                                // icons + filename                     + subtitle
                                // 1+1+1   width - (icons + substitle)     2+18
                                // if sort by filename
                                //  icons + width - icons

                                let max_subtitle_length = 16;
                                let max_filename_length = match self.sort_type {
                                    SortType::FileName => file_list_width - 2 - 4, // border - icon columns (unicode is two columns)
                                    _ => file_list_width - 2 - 4 - max_subtitle_length, // border - icon columns - spacer between subtitle
                                };

                                let file_name_display = if original_file_name.len()
                                    >= max_filename_length
                                {
                                    format!("{}..", &original_file_name[..max_filename_length - 2])
                                } else {
                                    format!(
                                        "{:<width$}",
                                        original_file_name,
                                        width = max_filename_length - 1
                                    )
                                };

                                let entry_text = Span::styled(
                                    file_name_display,
                                    Style::default()
                                        .bg(self.get_color(ThemeColor::SelectedBG))
                                        .fg(fg_color)
                                        .add_modifier(Modifier::BOLD),
                                );

                                let entry_symbol = match file.trashroot.root_type {
                                    TrashRootType::Home => Span::from("  "),
                                    _ => Span::from("🢅 "),
                                };

                                let subtitle = match self.sort_type {
                                    SortType::DeletionDate => {
                                        let now = Local::now();
                                        let diff = now
                                            - file.trashinfo.clone().unwrap().get_deletion_date();
                                        if diff.num_days() != 0 {
                                            format!("{} days ago", diff.num_days())
                                        } else if diff.num_hours() != 0 {
                                            format!("{} hours ago", diff.num_hours())
                                        } else if diff.num_minutes() != 0 {
                                            format!("{} minutes ago", diff.num_minutes())
                                        } else {
                                            format!("{} seconds ago", diff.num_seconds())
                                        }
                                    }
                                    SortType::TrashRoot => {
                                        if original_path_display.len() > max_subtitle_length {
                                            format!(
                                                "{:>width$}..",
                                                &original_path_display[..max_subtitle_length - 2],
                                                width = max_subtitle_length - 2
                                            )
                                        } else {
                                            original_path_display
                                        }
                                    }
                                    SortType::Size => format!(
                                        "{:>width$}",
                                        f_size_display,
                                        width = max_subtitle_length - 1
                                    ),
                                    SortType::FileName => "".to_string(),
                                };

                                let subtitle_span = Span::styled(
                                    format!(
                                        "{:>width$} ",
                                        subtitle,
                                        width = max_subtitle_length - 1
                                    ),
                                    Style::default()
                                        .fg(self.get_color(ThemeColor::SelectedFGFile))
                                        .add_modifier(Modifier::ITALIC),
                                );

                                Line::from(vec![
                                    entry_symbol,
                                    entry_filetype,
                                    entry_text,
                                    subtitle_span,
                                ])
                                .style(Style::default().bg(self.get_color(ThemeColor::SelectedBG)))
                            } else {
                                let (fg_color, entry_filetype) =
                                    if file.files_entry.as_ref().unwrap().is_symlink() {
                                        (
                                            self.get_color(ThemeColor::UnselectedFGLink),
                                            Span::from("🔗"),
                                        )
                                    } else if file.files_entry.as_ref().unwrap().is_dir() {
                                        (
                                            self.get_color(ThemeColor::UnselectedFGDir),
                                            Span::from("📁"),
                                        )
                                    } else {
                                        (
                                            self.get_color(ThemeColor::UnselectedFGFile),
                                            Span::from("📄"),
                                        )
                                    };

                                let max_filename_length = file_list_width - 2 - 4; // border - icon columns
                                let file_name_display = if original_file_name.len()
                                    >= max_filename_length
                                {
                                    format!("{}..", &original_file_name[..max_filename_length - 2])
                                } else {
                                    format!("{original_file_name:<max_filename_length$}")
                                };

                                let entry_text =
                                    Span::styled(file_name_display, Style::default().fg(fg_color));
                                let entry_symbol = match file.trashroot.root_type {
                                    TrashRootType::Home => Span::from("  "),
                                    _ => Span::from("🢅 "),
                                };
                                Line::from(vec![entry_symbol, entry_filetype, entry_text])
                            };

                            ListItem::new(entry)
                        })
                        .collect();

                    // for the right side title
                    let sort_value = match self.sort_type {
                        SortType::DeletionDate => "[Deleted On ↑]",
                        SortType::TrashRoot => "[Original Path A-Z]",
                        SortType::Size => "[File Size ↑]",
                        SortType::FileName => "[File Name A-Z]",
                    };

                    let list = List::new(list_items).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(Span::styled(
                                format!(
                                    " Files in Trash [{}/{}] ",
                                    self.selected + 1,
                                    total_item_count,
                                ),
                                title_style,
                            ))
                            .title_top(
                                Line::from(vec![
                                    Span::styled(
                                        " Sorted By ",
                                        title_style.remove_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        format!("{sort_value} ").to_string(),
                                        title_style.add_modifier(Modifier::ITALIC),
                                    ),
                                ])
                                .right_aligned(),
                            )
                            .style(block_style),
                    );
                    f.render_widget(list, midsection_columns[0]);
                }

                // ============= right column
                let right_column_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(
                        [
                            Constraint::Percentage(100 - LAYOUT_PREVIEW_HEIGHT_PERCENTAGE),
                            Constraint::Percentage(LAYOUT_PREVIEW_HEIGHT_PERCENTAGE),
                        ]
                        .as_ref(),
                    )
                    .split(midsection_columns[1]);

                // -------------------- description
                let desc_block = Block::default()
                    .title(Span::styled(" Description ", title_style))
                    .borders(Borders::ALL)
                    .style(block_style)
                    .padding(Padding::new(1, 1, 1, 1));
                let desc_text = Paragraph::new(selected_desc)
                    .wrap(Wrap { trim: false })
                    .block(desc_block);

                f.render_widget(desc_text, right_column_chunks[0]);

                // -------------------- preview
                let preview_block = Block::default()
                    .title(Span::styled(" Preview ", title_style))
                    .borders(Borders::ALL)
                    .style(block_style)
                    .padding(Padding::new(1, 1, 1, 1));
                let preview_text = Paragraph::new(preview).block(preview_block);

                f.render_widget(preview_text, right_column_chunks[1]);

                // ---------------------- scroll bar for the list
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
                f.render_stateful_widget(scrollbar, midsection_columns[0], &mut scrollbar_state);

                // -------------------- shortcuts
                directions = Line::from(vec![
                    Span::styled("h/f1", title_style),
                    Span::styled(" - help ", Style::default()),
                    Span::styled("↓↑/jk", title_style),
                    Span::styled(" - navigate list, ", Style::default()),
                    Span::styled("enter", title_style),
                    Span::styled(" - restore, ", Style::default()),
                    Span::styled("del", title_style),
                    Span::styled(" - del, ", Style::default()),
                    Span::styled("shift + del", title_style),
                    Span::styled(" - empty trash bin, ", Style::default()),
                    Span::styled("q", title_style),
                    Span::styled(" - quit, ", Style::default()),
                    Span::styled("s", title_style),
                    Span::styled(" - sort", Style::default()),
                ]);
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
                        dialog_text_style.add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("to ", dialog_text_style),
                    Span::styled(
                        format!("'{}' ", selected_file.original_file.display()),
                        dialog_text_style.add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("?", dialog_text_style),
                ]);

                // space between buttons
                let spacer = Span::styled("      ", dialog_text_style);

                // illusion of buttons
                let buttons = if *choice == 0 {
                    Line::from(vec![
                        Span::styled("[Confirm]", dialog_button_selected_style),
                        spacer,
                        Span::styled("[Cancel]", dialog_button_unseleted_style),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled("[Confirm]", dialog_button_unseleted_style),
                        spacer,
                        Span::styled("[Cancel]", dialog_button_selected_style),
                    ])
                };

                // popup dialog
                let area = f.area();
                let block = Block::bordered()
                    .title(Span::styled(
                        "Confirm Restore",
                        dialog_text_style.add_modifier(Modifier::BOLD),
                    ))
                    .style(dialog_style);
                let area = popup_area(area, 40, 15);
                let dialog = Paragraph::new(vec![question, Line::from(vec![]), buttons])
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area);
                f.render_widget(dialog, area);

                directions = Line::from(vec![
                    Span::styled("←→/hl", title_style),
                    Span::styled(" - select, ", Style::default()),
                    Span::styled("enter", title_style),
                    Span::styled(" - confirm selection, ", Style::default()),
                    Span::styled("q/esc", title_style),
                    Span::styled(" - go back ", Style::default()),
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
                        dialog_text_style.add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" forever?", dialog_text_style),
                ]);

                // space between buttons
                let spacer = Span::styled("      ", Style::default());

                // illusion of buttons
                let buttons = if *choice == 0 {
                    Line::from(vec![
                        Span::styled("[Confirm]", dialog_button_selected_style),
                        spacer,
                        Span::styled("[Cancel]", dialog_button_unseleted_style),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled("[Confirm]", dialog_button_unseleted_style),
                        spacer,
                        Span::styled("[Cancel]", dialog_button_selected_style),
                    ])
                };

                // popup dialog
                let area = f.area();
                let block = Block::bordered()
                    .title(Span::styled(
                        "Confirm Deletion",
                        dialog_text_style.add_modifier(Modifier::BOLD),
                    ))
                    .style(dialog_style);
                let area = popup_area(area, 40, 15);
                let dialog = Paragraph::new(vec![question, Line::from(vec![]), buttons])
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area);
                f.render_widget(dialog, area);

                directions = Line::from(vec![
                    Span::styled("←→/hl", title_style),
                    Span::styled(" - select, ", Style::default()),
                    Span::styled("enter", title_style),
                    Span::styled(" - confirm selection, ", Style::default()),
                    Span::styled("q/esc", title_style),
                    Span::styled(" - go back ", Style::default()),
                ]);
            }

            AppState::EmptyBinConfirmation(choice) => {
                // question in some mixed style
                let question = Line::from(vec![Span::styled(
                    "This will permanently delete ALL files in the trash bin forever",
                    dialog_text_style,
                )]);

                // space between buttons
                let spacer = Span::styled("      ", Style::default());

                // illusion of buttons
                let buttons = if *choice == 0 {
                    Line::from(vec![
                        Span::styled("[Confirm]", dialog_button_selected_style),
                        spacer,
                        Span::styled("[Cancel]", dialog_button_unseleted_style),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled("[Confirm]", dialog_button_unseleted_style),
                        spacer,
                        Span::styled("[Cancel]", dialog_button_selected_style),
                    ])
                };

                // popup dialog
                let area = f.area();
                let block = Block::bordered()
                    .title(Span::styled(
                        "Confirm Empty Bin",
                        dialog_text_style.add_modifier(Modifier::BOLD),
                    ))
                    .style(dialog_style);
                let area = popup_area(area, 30, 10);
                let dialog = Paragraph::new(vec![question, Line::from(vec![]), buttons])
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area);
                f.render_widget(dialog, area);

                directions = Line::from(vec![
                    Span::styled("←→/hl", title_style),
                    Span::styled(" - select, ", Style::default()),
                    Span::styled("enter", title_style),
                    Span::styled(" - confirm selection, ", Style::default()),
                    Span::styled("q/esc", title_style),
                    Span::styled(" - go back ", Style::default()),
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
                    Span::styled("[x]", dialog_button_selected_style)
                } else {
                    Span::styled("[ ]", dialog_button_unseleted_style)
                };

                let dd_label = if *choice == SortType::DeletionDate {
                    Span::styled(" Deleted on", dialog_button_selected_style)
                } else {
                    Span::styled(" Deleted on", dialog_button_unseleted_style)
                };

                choices.push(Line::from(vec![dd_check_mark, dd_label]));

                // Origin
                let o_check_mark = if self.sort_type == SortType::TrashRoot {
                    Span::styled("[x]", dialog_button_selected_style)
                } else {
                    Span::styled("[ ]", dialog_button_unseleted_style)
                };

                let o_label = if *choice == SortType::TrashRoot {
                    Span::styled(" Origin    ", dialog_button_selected_style)
                } else {
                    Span::styled(" Origin    ", dialog_button_unseleted_style)
                };

                choices.push(Line::from(vec![o_check_mark, o_label]));

                // Size
                let s_check_mark = if self.sort_type == SortType::Size {
                    Span::styled("[x]", dialog_button_selected_style)
                } else {
                    Span::styled("[ ]", dialog_button_unseleted_style)
                };

                let s_label = if *choice == SortType::Size {
                    Span::styled(" Size      ", dialog_button_selected_style)
                } else {
                    Span::styled(" Size      ", dialog_button_unseleted_style)
                };

                choices.push(Line::from(vec![s_check_mark, s_label]));

                // file name
                let fn_check_mark = if self.sort_type == SortType::FileName {
                    Span::styled("[x]", dialog_button_selected_style)
                } else {
                    Span::styled("[ ]", dialog_button_unseleted_style)
                };

                let fn_label = if *choice == SortType::FileName {
                    Span::styled(" File Name ", dialog_button_selected_style)
                } else {
                    Span::styled(" File Name ", dialog_button_unseleted_style)
                };

                choices.push(Line::from(vec![fn_check_mark, fn_label]));

                // popup dialog
                let mut dialog_content = vec![question, Line::from(vec![])];
                dialog_content.append(&mut choices);

                let area = f.area();
                let block = Block::bordered()
                    .title(Span::styled(
                        "Sort Files By",
                        dialog_text_style.add_modifier(Modifier::BOLD),
                    ))
                    .style(dialog_style);
                let area = popup_area(area, 30, 15);
                let dialog = Paragraph::new(dialog_content)
                    .wrap(Wrap { trim: false })
                    .alignment(Alignment::Center)
                    .block(block);
                f.render_widget(Clear, area);
                f.render_widget(dialog, area);

                directions = Line::from(vec![
                    Span::styled("↓↑/jk", title_style),
                    Span::styled(" - select, ", Style::default()),
                    Span::styled("enter", title_style),
                    Span::styled(" - confirm selection, ", Style::default()),
                    Span::styled("q/esc", title_style),
                    Span::styled(" - go back ", Style::default()),
                ]);
            }

            AppState::HelpScreen => {
                let area = f.area();
                let block = Block::bordered()
                    .title(Span::styled(
                        "Help",
                        dialog_text_style.add_modifier(Modifier::BOLD),
                    ))
                    .padding(Padding::new(2, 2, 2, 1))
                    .style(dialog_style);

                let empty_line = Line::default();
                let shortcut_style = dialog_text_style.add_modifier(Modifier::BOLD);
                let dash = Span::from(" - ");
                let desc_style = dialog_text_style.add_modifier(Modifier::ITALIC);

                let shortcuts_list = vec![
                    Line::from(format!("{BINARY_NAME} is a freedesktop.org Trash Specification implementation written in Rust. Current version is {BINARY_VERSION}.")),
                    Line::from(format!("{BINARY_NAME} is an Open Source tool licensed under Apache License v2.")),
                    empty_line.clone(),
                    Line::from("http://www.apache.org/licenses/LICENSE-2.0"),
                    empty_line.clone(),
                    Line::styled("Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an \"AS IS\" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and limitations under the License.", desc_style),
                    empty_line.clone(),
                    Line::from(vec![Span::from("Please report any issues to "),
                        Span::styled(
                        "https://github.com/chamilad/trash-rs",
                        shortcut_style,
                    )]),
                    empty_line.clone(),
                    empty_line.clone(),
                    Line::from(vec![
                        Span::styled(
                        "Keyboard Shortcuts [Case Sensitive]",
                        shortcut_style,
                    )]),
                    Line::from(vec![
                        Span::styled(
                        "-----------------------------------",
                        shortcut_style,
                    )]),
                    empty_line.clone(),
                    Line::from(vec![
                        Span::styled("↓↑/jk        ", shortcut_style),
                        dash.clone(),
                        Span::styled("navigate file list", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("↵ (enter)    ", shortcut_style),
                        dash.clone(),
                        Span::styled(
                            "restore file, select option (when a dialog is open)",
                            desc_style,
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("del          ", shortcut_style),
                        dash.clone(),
                        Span::styled("delete file", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("shift + del  ", shortcut_style),
                        dash.clone(),
                        Span::styled("empty trash bin", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("s            ", shortcut_style),
                        dash.clone(),
                        Span::styled("open sort by dialog", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("r/f5         ", shortcut_style),
                        dash.clone(),
                        Span::styled("refresh file list", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("g/pageup     ", shortcut_style),
                        dash.clone(),
                        Span::styled("go to the top in the list", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("G/pagedown   ", shortcut_style),
                        dash.clone(),
                        Span::styled("go to the bottom in the list", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("h/f1         ", shortcut_style),
                        dash.clone(),
                        Span::styled("show this screen (good job!)", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("q            ", shortcut_style),
                        dash.clone(),
                        Span::styled(
                            "exit (when in main screen), close dialog (when a dialog is open)",
                            desc_style,
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("escape       ", shortcut_style),
                        dash.clone(),
                        Span::styled("close dialog (only when a dialog is open)", desc_style),
                    ]),
                    Line::from(vec![
                        Span::styled("←→↓↑/hljk/tab", shortcut_style),
                        dash.clone(),
                        Span::styled(
                            "select button/option (only when a dialog is open)",
                            desc_style,
                        ),
                    ]),
                ];

                let shortcuts = Paragraph::new(shortcuts_list)
                    .wrap(Wrap { trim: false })
                    .block(block);

                let area = popup_area(area, 60, 60);
                f.render_widget(Clear, area);
                f.render_widget(shortcuts, area);

                directions = Line::from(vec![
                    Span::styled("q/esc", title_style),
                    Span::styled(" - go back ", Style::default()),
                ]);
            }

            _ => {}
        }

        // ================== footer
        let footer_block = Block::default().borders(Borders::ALL).style(block_style);
        let directions_block = Paragraph::new(directions).block(footer_block);
        f.render_widget(directions_block, main_horizontal_blocks[2]);
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
                    // go to absolute top
                    self.selected = 0;
                    self.scroll_offset = 0;
                }
                KeyCode::Char('G') | KeyCode::PageDown => {
                    // go to absolute bottom
                    self.selected = self.trashed_files.len() - 1;
                    self.scroll_offset = self.selected - self.max_visible_items + 1;
                }
                KeyCode::Char('h') | KeyCode::F(1) => {
                    self.state = AppState::HelpScreen;
                }
                KeyCode::Char('q') => {
                    self.state = AppState::Exiting;
                }
                _ => {}
            },

            AppState::RestoreConfirmation(choice) => {
                match key.code {
                    KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('l')
                    | KeyCode::Char('h')
                    | KeyCode::Tab => {
                        // toggle between Yes (0) and No (1)
                        if let AppState::RestoreConfirmation(choice) = &mut self.state {
                            *choice = if *choice == 0 { 1 } else { 0 };
                        }
                    }
                    KeyCode::Enter => {
                        // confirm the action if Yes is selected
                        if choice == 0 {
                            let selected_file = &self.trashed_files[self.selected];
                            let _ = selected_file.restore().expect("could not restore file");
                        }

                        // refresh and return to file list after action or cancel
                        self.state = AppState::RefreshFileList;
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // close the dialog without performing any action
                        self.state = AppState::RefreshFileList;
                    }
                    _ => {}
                }
            }

            AppState::DeletionConfirmation(choice) => {
                match key.code {
                    KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('l')
                    | KeyCode::Char('h')
                    | KeyCode::Tab => {
                        // toggle between Yes (0) and No (1)
                        if let AppState::DeletionConfirmation(choice) = &mut self.state {
                            *choice = if *choice == 0 { 1 } else { 0 };
                        }
                    }
                    KeyCode::Enter => {
                        // confirm the action if Yes is selected
                        if choice == 0 {
                            let selected_file = &self.trashed_files[self.selected];
                            selected_file
                                .delete_forever()
                                .expect("could not delete file");
                        }

                        // refresh and return to file list after action or cancel
                        self.state = AppState::RefreshFileList;
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // close the dialog without performing any action
                        self.state = AppState::RefreshFileList;
                    }
                    _ => {}
                }
            }

            AppState::EmptyBinConfirmation(choice) => {
                match key.code {
                    KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('l')
                    | KeyCode::Char('h')
                    | KeyCode::Tab => {
                        // toggle between Yes (0) and No (1)
                        if let AppState::DeletionConfirmation(choice) = &mut self.state {
                            *choice = if *choice == 0 { 1 } else { 0 };
                        }
                    }
                    KeyCode::Enter => {
                        // confirm the action if Yes is selected
                        if choice == 0 {
                            for trash_file in &self.trashed_files {
                                // one error shouldn't stop operation
                                if trash_file.delete_forever().is_err() {
                                    // todo: show an error notification
                                }
                            }
                        }

                        // refresh and return to file list after action or cancel
                        self.state = AppState::RefreshFileList;
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // close the dialog without performing any action
                        self.state = AppState::RefreshFileList;
                    }
                    _ => {}
                }
            }

            AppState::HelpScreen => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // close the dialog without performing any action
                        self.state = AppState::RefreshFileList;
                    }
                    _ => {}
                }
            }

            AppState::SortListDialog(choice) => match key.code {
                KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Tab
                | KeyCode::Char('l')
                | KeyCode::Right => {
                    let next_choice = match choice {
                        SortType::DeletionDate => SortType::TrashRoot,
                        SortType::TrashRoot => SortType::Size,
                        SortType::Size => SortType::FileName,
                        SortType::FileName => SortType::FileName,
                    };
                    self.state = AppState::SortListDialog(next_choice);
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('h') | KeyCode::Left => {
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

    // select color based on the current theme
    fn get_color(&self, color: ThemeColor) -> Color {
        match self.theme {
            Theme::Dark => match color {
                ThemeColor::Highlight => Color::White,
                ThemeColor::TitleText => Color::Black,
                ThemeColor::Text => Color::Gray,
                ThemeColor::BoldText => Color::White,
                ThemeColor::ErrorText => Color::LightRed,
                ThemeColor::SelectedFGDir => Color::Blue,
                ThemeColor::SelectedFGLink => Color::Magenta,
                ThemeColor::SelectedFGFile => Color::White,
                ThemeColor::SelectedBG => Color::DarkGray,
                ThemeColor::UnselectedFGDir => Color::Blue,
                ThemeColor::UnselectedFGLink => Color::Magenta,
                ThemeColor::UnselectedFGFile => Color::White,
                ThemeColor::DialogBG => Color::Gray,
                ThemeColor::DialogText => Color::Black,
                ThemeColor::DialogButtonBG => Color::Black,
                ThemeColor::DialogButtonText => Color::White,
            },
            Theme::Light => match color {
                ThemeColor::Highlight => Color::DarkGray,
                ThemeColor::TitleText => Color::White,
                ThemeColor::Text => Color::DarkGray,
                ThemeColor::BoldText => Color::Black,
                ThemeColor::ErrorText => Color::LightRed,
                ThemeColor::SelectedFGDir => Color::LightBlue,
                ThemeColor::SelectedFGLink => Color::LightMagenta,
                ThemeColor::SelectedFGFile => Color::Black,
                ThemeColor::SelectedBG => Color::Gray,
                ThemeColor::UnselectedFGDir => Color::Blue,
                ThemeColor::UnselectedFGLink => Color::Magenta,
                ThemeColor::UnselectedFGFile => Color::Black,
                ThemeColor::DialogBG => Color::DarkGray,
                ThemeColor::DialogText => Color::White,
                ThemeColor::DialogButtonBG => Color::White,
                ThemeColor::DialogButtonText => Color::Black,
            },
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let theme = match env::var("TRASH_RS_THEME") {
        Ok(v) => match v.to_uppercase().trim() {
            "LIGHT" => Theme::Light,
            _ => Theme::Dark,
        },
        Err(_) => Theme::Dark,
    };

    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;

    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(theme);

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

        if let Event::Key(key) = event::read()? {
            // ratatui records press and release
            if key.kind == event::KeyEventKind::Release {
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

// collect trashed files from home mount and other devices mounted as readable
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

// sort a given vector of files based on the sort type
//
// opinionated on the order,
// date latest>oldest
// root dev_id
// size largest>smallest
// filename a-z
fn sort_file_list(list: &mut [TrashFile], sort_by: &SortType) {
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
            let cmp_dev = b_dev.cmp(&a_dev);
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
