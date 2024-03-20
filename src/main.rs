use chrono;
use std::env;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const EXITCODE_INVALID_ARGS: i32 = 1;
const EXITCODE_UNSUPPORTED: i32 = 2;
const EXITCODE_UNKNOWN: i32 = 255;

// const TRASHINFO_TEMPLATE: &str = "[Trash Info]
// Path={}
// DeletionDate={}
// ";

fn main() -> Result<(), Box<dyn Error>> {
    // parse the args
    let args: Vec<String> = env::args().collect();

    // if there's just one arg, that should be the filename, not a flag
    if args.len() < 2 {
        eprintln!("error: missing file name");
        std::process::exit(EXITCODE_INVALID_ARGS);
    }

    if args[args.len() - 1].starts_with("-") {
        eprintln!("error: missing file name");
        std::process::exit(EXITCODE_INVALID_ARGS);
    }

    // let flags: Vec<String> = Vec::new();
    // todo: last item is file for now
    let file_path_arg: &String = &args[args.len() - 1];

    // todo: clean args
    // check all but last arguments are flags
    // for arg in &args[1..args.len()-2] {

    // }

    // 1. check if file/dir exists
    //
    // dbg!(args);

    // check if file exists
    let file_path = Path::new(&file_path_arg);
    // if !file_path.is_absolute() {
    // file_path = Path
    // }
    let metadata = match file_path.metadata() {
        Ok(m) => m,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                eprintln!("error: specified file doesn't exist");
                std::process::exit(EXITCODE_INVALID_ARGS);
            } else {
                dbg!(e);
                eprintln!("error: error while checking file");
                std::process::exit(EXITCODE_UNKNOWN);
            }
        }
    };

    if metadata.is_dir() {
        eprintln!("error: directories not supported yet");
        std::process::exit(EXITCODE_UNSUPPORTED);
    }

    // get the trash dir
    let trash_path_buf = get_trash_dir(&file_path_arg)?;
    let trash_path: &Path = trash_path_buf.as_path();

    // check if same name exists
    let stripped_file_name = file_path.file_name().unwrap().to_str().unwrap();
    let trash_file_suffix =
        match get_trash_file_suffix(trash_path.to_str().unwrap(), stripped_file_name) {
            Some(v) => format!(".{}", v),
            None => "".to_string(),
        };

    let trash_file_name = format!("{}/files/{}{}", trash_path.to_str().unwrap(), stripped_file_name, trash_file_suffix);
    let trash_file = Path::new(&trash_file_name);

    // add an .trashinfo entry
    create_info(trash_path.to_str().unwrap(), file_path, trash_file)?;

    // move the file
    let file_abs = std::fs::canonicalize(file_path)?;
    move_to_trash(trash_path, &file_abs.as_path(), &trash_file)?;

    Ok(())
}

// derive trash directory according to trash spec
fn get_trash_dir(_: &String) -> Result<PathBuf, Box<dyn Error>> {
    let xdg_data_home = match env::var("XDG_DATA_HOME") {
        Ok(val) => val,
        Err(_) => {
            let home_dir = get_home_dir()?;
            format!("{}/.local/share", home_dir)
        }
    };

    // todo: check if topdir exists if file is in a mounted drive

    let trash_dir = format!("{}/Trash", xdg_data_home);
    let trash_dir_clone = trash_dir.clone();
    let trash_dir_path = Path::new(&trash_dir_clone);
    match trash_dir_path.metadata() {
        Ok(m) => {
            // todo: info and files dirs might not exist
            if m.is_dir() {
                println!("debug: trash dir: {}", trash_dir_path.to_str().unwrap());
                return Ok(trash_dir_path.to_path_buf());
            } else {
                return Err(Box::<dyn Error>::from("invalid Trash dir"));
            }
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                println!("debug: trash dir not found - {}", trash_dir);
                fs::create_dir(trash_dir.clone())?;
                fs::create_dir(format!("{}/info", trash_dir))?;
                fs::create_dir(format!("{}/files", trash_dir))?;
            }
        }
    }

    println!("debug: trash dir: {}", trash_dir_path.to_str().unwrap());
    Ok(trash_dir_path.to_path_buf())
}

fn get_home_dir() -> Result<String, Box<dyn Error>> {
    Ok(env::var("HOME")?)
}

// derive the suffix the trash file should have if duplicate files already exist in the trash
fn get_trash_file_suffix(trash_path: &str, file_name: &str) -> Option<i32> {
    let trash_file_name = format!("{}/{}", trash_path, file_name);
    let trash_file = Path::new(&trash_file_name);
    if !trash_file.exists() {
        return None;
    }

    for n in 2..1024 {
        let trash_file_name = format!("{}/{}.{}", trash_path, file_name, n);
        let trash_file_d = Path::new(&trash_file_name);
        if !trash_file_d.exists() {
            return Some(n);
        }
    }

    None
}

// create trashinfo entry
fn create_info(trash_dir: &str, orig_file: &Path, trash_file: &Path) -> Result<(), Box<dyn Error>> {
    must_have_info_dir(trash_dir)?;

    println!("debug: creating trashinfo: {}", trash_file.to_str().unwrap());
    let info_entry_file_name = format!(
        "{}/info/{}.trashinfo",
        trash_dir,
        trash_file.file_name().unwrap().to_str().unwrap()
    );
    let info_entry = Path::new(&info_entry_file_name);
    if info_entry.exists() {
        return Err(Box::<dyn Error>::from("info entry already exists"));
    }

    let full_file_path = std::fs::canonicalize(orig_file)?;
    let full_file_path_str = full_file_path.to_str().unwrap();
    let now = chrono::Local::now();
    let deletion_date = now.to_rfc3339_opts(chrono::format::SecondsFormat::Secs, true);
    let trashinfo = format!(
        r#"[Trash Info]
Path={}
DeletionDate={}
"#,
        full_file_path_str, deletion_date
    );

    println!("debug: creating trashinfo file: {}", trash_file.to_str().unwrap());
    let mut file = match std::fs::File::create(info_entry_file_name) {
        Ok(v) => v,
        Err(e) => {
            return Err(Box::<dyn Error>::from(format!(
                "error while creating trashinfo entry: {}",
                e
            )));
        }
    };

    println!("debug: writing to trashinfo: {}", trash_file.to_str().unwrap());
    match file.write_all(trashinfo.as_bytes()) {
        Ok(_) => println!("debug: trashinfo created"),
        Err(e) => {
            return Err(Box::<dyn Error>::from(format!(
                "error while writing to trashinfo file: {}",
                e
            )));
        }
    };

    Ok(())
}

// create info directory if it doesn't exist
fn must_have_info_dir(trash_path: &str) -> Result<(), Box<dyn Error>> {
    let info_dir_full_path = format!("{}/info", trash_path);
    let info_dir = Path::new(&info_dir_full_path);
    if !info_dir.is_dir() {
        fs::create_dir(info_dir_full_path)?;
    }
    Ok(())
}

fn must_have_files_dir(trash_path: &str) -> Result<(), Box<dyn Error>> {
    let files_dir_full_name = format!("{}/files", trash_path);
    let files_dir = Path::new(&files_dir_full_name);
    if !files_dir.is_dir() {
        fs::create_dir(files_dir_full_name)?;
    }

    Ok(())
}

fn move_to_trash(trash_path: &Path, orig_file: &Path, trash_file: &Path) -> Result<(), Box<dyn Error>> {
    must_have_files_dir(trash_path.to_str().unwrap())?;
    println!("moving {} to {}", orig_file.to_str().unwrap(), trash_file.to_str().unwrap());
    fs::rename(orig_file.to_str().unwrap(), trash_file.to_str().unwrap())?;
    Ok(())
}
