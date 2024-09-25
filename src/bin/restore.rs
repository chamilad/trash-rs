use std::env;
use std::error::Error;
use std::fs::{read_dir, read_to_string};
use std::path::PathBuf;

use crossterm::event::{self, Event, KeyCode};
use libtrash::*;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::DisableMouseCapture;
use ratatui::crossterm::event::EnableMouseCapture;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Terminal;
use std::fs;
use std::io;
use std::path::Path;
use urlencoding::decode;

const VERBOSE_MODE: bool = false;

struct TrashedFile {
    OriginalFile: PathBuf,
    DeletionDate: String,
    File: PathBuf,
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
    let mut selected_index = 0;
    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100)].as_ref())
                .split(f.area());

            let list_items: Vec<ListItem> = files
                .iter()
                .enumerate()
                .map(|(i, file)| {
                    // for trashed_file in files {
                    // println!(
                    //     "{} \t {} \t {}",
                    //     trashed_file
                    //         .OriginalFile
                    //         .file_name()
                    //         .expect("file-name")
                    //         .to_str()
                    //         .expect("file-name"),
                    //     trashed_file.DeletionDate,
                    //     trashed_file.OriginalFile.display(),
                    // );
                    let original_path = file
                        .OriginalFile
                        .file_name()
                        .expect("file_name")
                        .to_os_string()
                        .into_string()
                        .unwrap();

                    let entry = if i == selected_index {
                        Span::styled(
                            original_path,
                            Style::default()
                                .bg(Color::Yellow)
                                .fg(Color::Black)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::raw(original_path)
                    };
                    // let entry = Span::raw(original_path);
                    // let item = ListItem::new(entry);
                    ListItem::new(entry)
                    // list_items.push(item);
                })
                .collect();

            let list = List::new(list_items)
                .block(Block::default().borders(Borders::ALL).title("Trash"))
                .highlight_style(Style::default().fg(Color::Yellow));

            f.render_widget(list, chunks[0]);
        })?;

        // Handle input events
        // if event::poll(std::time::Duration::from_millis(100))? {
        if let Event::Key(key) = event::read()? {
            if key.kind == event::KeyEventKind::Release {
                // Skip events that are not KeyEventKind::Press
                continue;
            }

            match key.code {
                KeyCode::Up => {
                    if selected_index > 0 {
                        selected_index -= 1;
                    }
                }
                KeyCode::Down => {
                    if selected_index < files.len() - 1 {
                        selected_index += 1;
                    }
                }
                KeyCode::Enter => {
                    let selected_file = &files[selected_index];
                    println!(
                        "Executing action on: {}",
                        selected_file
                            .OriginalFile
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                    );
                    // Here you can define the action for the file (e.g., open the file, run a command, etc.)
                }
                KeyCode::Char('q') => {
                    // Exit on 'q'
                    break;
                }
                _ => {}
            }
        }
        // }
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

    // let trashed_file_name = args[1];
    // figure out trash root for specified file
    //  is not a problem if I go for the tui now, root can be metadata per entry
    // match filename with trashinfo
    // parse trashinfo
    // todo: if parent directory doesn't exist anymore, show error
    // confirm from user
    // move file to original location
    // }
    // _ => {
    //     println!("unsupported command: {command}");
    //     std::process::exit(1);
    // }
    // }
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
