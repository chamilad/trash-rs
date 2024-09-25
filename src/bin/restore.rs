use std::env;
use std::error::Error;
use std::fs::{read_dir, read_to_string};
use std::path::PathBuf;

use libtrash::*;
use urlencoding::decode;

const VERBOSE_MODE: bool = false;
const CMD_LIST: &str = "list";
const CMD_RESTORE: &str = "restore";

struct TrashedFile {
    OriginalFile: PathBuf,
    DeletionDate: String,
    File: PathBuf,
}

fn main() {
    // list trash
    // let args: Vec<String> = env::args().skip(1).collect();
    // let command = args[0].clone();
    // match command.as_str() {
    //     // CMD_LIST => {
    //
    // get user trash directory
    let user_home = get_home_dir().expect("couldn't get user home directory");
    let user_trash_dir = TrashDirectory::resolve_for_file(&user_home, VERBOSE_MODE)
        .expect("couldn't resolve user home trash dir");

    // iterate through entries in files and read the matching trashinfo, show the filename based on the entry
    // in trashinfo
    let home_files: Vec<TrashedFile> =
        get_trashed_files(user_trash_dir).expect("error while iterating trash files");
    //
    // todo: do the same for every mounted drive

    for trashed_file in home_files {
        println!(
            "{} \t {} \t {}",
            trashed_file
                .OriginalFile
                .file_name()
                .expect("file-name")
                .to_str()
                .expect("file-name"),
            trashed_file.DeletionDate,
            trashed_file.OriginalFile.display(),
        );
    }

    // }
    // CMD_RESTORE => {
    // if args.len() < 2 {
    //     msg_err("missing filename");
    //     std::process::exit(1); //todo
    // }

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
        // println!(
        //     "{} \t {} \t {}",
        //     original_file
        //         .file_name()
        //         .expect("file-name")
        //         .to_str()
        //         .expect("file-name"),
        //     deletion_date,
        //     original_path
        // );
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
